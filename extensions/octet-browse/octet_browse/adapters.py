"""Explicit browser-session connector contracts for octet Browse.

The default Browse runtime owns an isolated Chromium context.  This module only
provides an opt-in boundary for integrations that already own a browser
connection (for example Luna/max or a Playwright MCP bridge).  It deliberately
does not contain browser discovery, process enumeration, profile handling, or
cookie/storage access.
"""

from __future__ import annotations

import threading
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, Iterable, List, Mapping, Optional, Tuple

from .safety import BrowseError, ResourceOwner, bounded_text


MAX_BACKEND_ID_CHARS = 128
MAX_SELECTION_ID_CHARS = 256
MAX_BACKENDS = 16

_SELECTION_FIELDS = (
    "connector_id",
    "browser_id",
    "session_id",
    "window_id",
    "tab_id",
)

# These are operation capabilities, not permissions to bypass Browse policy.
_CAPABILITY_NAMES = (
    "snapshot",
    "click",
    "type",
    "press",
    "scroll",
    "wait",
    "screenshot",
    "navigation",
    "tab_close",
    "new_tab",
    "popup",
    "window_resize",
    "cookies",
    "storage",
)


def _selection_id(value: Any, name: str, *, maximum: int = MAX_SELECTION_ID_CHARS) -> str:
    if not isinstance(value, str) or not value or len(value) > maximum:
        raise BrowseError(
            "invalid_backend_selection",
            "%s must be a bounded explicit browser identity." % name,
        )
    if value != value.strip() or any(ord(character) < 32 or ord(character) == 127 for character in value):
        raise BrowseError(
            "invalid_backend_selection",
            "%s contains unsafe characters." % name,
        )
    return value


def _connector_id(value: Any) -> str:
    return _selection_id(value, "connector_id", maximum=MAX_BACKEND_ID_CHARS)


def _browser_family(value: Any) -> str:
    if not isinstance(value, str):
        return "unknown"
    normalized = value.strip().lower()
    if normalized in {"chromium", "chrome", "google-chrome", "edge", "msedge"}:
        return "chromium"
    if normalized in {"firefox", "gecko"}:
        return "firefox"
    if normalized in {"safari", "webkit"}:
        return "safari"
    return normalized or "unknown"


def _page_is_closed(page: Any) -> bool:
    try:
        probe = getattr(page, "is_closed", None)
    except Exception:
        return True
    if probe is None:
        return False
    try:
        return bool(probe() if callable(probe) else probe)
    except Exception:
        return True


def _safe_reason(value: Any, default: str) -> str:
    if not isinstance(value, str) or not value:
        return default
    return bounded_text(value, 256)


def _capabilities(
    value: Optional[Mapping[str, Any]], *, supported: bool, reason: str
) -> Dict[str, Dict[str, Any]]:
    result: Dict[str, Dict[str, Any]] = {}
    for name in _CAPABILITY_NAMES:
        raw = value.get(name) if isinstance(value, Mapping) else None
        if isinstance(raw, Mapping):
            enabled = bool(raw.get("supported", False)) if supported else False
            detail = _safe_reason(raw.get("reason"), reason if not enabled else "")
        elif isinstance(raw, bool):
            enabled = raw if supported else False
            detail = "" if enabled else reason
        else:
            # Existing-tab integrations can safely reuse the Browse operation
            # policy, but never infer capabilities such as windows or popups.
            enabled = supported and name in {
                "snapshot",
                "click",
                "type",
                "press",
                "scroll",
                "wait",
                "screenshot",
                "tab_close",
            }
            detail = "" if enabled else reason
        result[name] = {"supported": enabled}
        if detail:
            result[name]["reason"] = bounded_text(detail, 256)
    return result


@dataclass(frozen=True)
class TargetSelection:
    """The complete identity required to attach to one existing target."""

    connector_id: str
    browser_id: str
    session_id: str
    window_id: str
    tab_id: str
    target_revision: Optional[str] = None

    def __post_init__(self) -> None:
        object.__setattr__(self, "connector_id", _connector_id(self.connector_id))
        for field_name in _SELECTION_FIELDS[1:]:
            object.__setattr__(
                self,
                field_name,
                _selection_id(getattr(self, field_name), field_name),
            )
        if self.target_revision is not None:
            object.__setattr__(
                self,
                "target_revision",
                _selection_id(self.target_revision, "target_revision"),
            )

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> "TargetSelection":
        if not isinstance(value, Mapping):
            raise BrowseError(
                "invalid_backend_selection",
                "An explicit connector, browser, session, window, and tab identity is required.",
            )
        return cls(
            connector_id=value.get("connector_id"),
            browser_id=value.get("browser_id"),
            session_id=value.get("session_id"),
            window_id=value.get("window_id"),
            tab_id=value.get("tab_id"),
            target_revision=value.get("target_revision"),
        )

    @property
    def identity(self) -> Tuple[str, str, str, str, str, str]:
        return (
            self.connector_id,
            self.browser_id,
            self.session_id,
            self.window_id,
            self.tab_id,
            self.target_revision or "",
        )

    @property
    def target_identity(self) -> Tuple[str, str, str, str, str]:
        return (
            self.connector_id,
            self.browser_id,
            self.session_id,
            self.window_id,
            self.tab_id,
        )

    def as_dict(self) -> Dict[str, str]:
        value: Dict[str, str] = {
            "connector_id": self.connector_id,
            "browser_id": self.browser_id,
            "session_id": self.session_id,
            "window_id": self.window_id,
            "tab_id": self.tab_id,
        }
        if self.target_revision is not None:
            value["target_revision"] = self.target_revision
        return value


# Names used by integrations that describe the selected object as a target.
BackendSelection = TargetSelection
SelectedTarget = TargetSelection


@dataclass(frozen=True)
class PlaywrightTarget:
    """A connector-returned Playwright page with an identity proof.

    ``page`` and ``context`` are intentionally opaque to the connector
    registry.  The attached engine only uses the selected page and never
    enumerates the supplied context's other pages.
    """

    selection: TargetSelection
    page: Any
    context: Any = None
    browser_family: str = "chromium"
    target_revision: Optional[str] = None
    metadata: Mapping[str, Any] = field(default_factory=dict)

    def __post_init__(self) -> None:
        if self.page is None:
            raise BrowseError(
                "backend_invalid_target",
                "The selected browser connector did not return a Playwright page.",
            )
        family = _browser_family(self.browser_family)
        object.__setattr__(self, "browser_family", family)
        if self.target_revision is not None:
            object.__setattr__(
                self,
                "target_revision",
                _selection_id(self.target_revision, "target_revision"),
            )

    @classmethod
    def from_connector_result(
        cls,
        value: Any,
        selection: TargetSelection,
        browser_family: str,
    ) -> "PlaywrightTarget":
        if isinstance(value, cls):
            target = value
            if target.selection.target_identity != selection.target_identity:
                raise BrowseError(
                    "backend_invalid_target",
                    "The connector returned a target different from the explicitly selected identity.",
                )
            if selection.target_revision and target.target_revision != selection.target_revision:
                raise BrowseError(
                    "stale_target",
                    "The selected browser target revision is stale; inspect backend status and select it again.",
                )
            if target.browser_family != _browser_family(browser_family):
                raise BrowseError(
                    "backend_invalid_target",
                    "The connector returned a browser family different from its declaration.",
                )
            if _page_is_closed(target.page):
                raise BrowseError(
                    "stale_target",
                    "The explicitly selected browser tab is already closed or unavailable.",
                )
            return target
        if not isinstance(value, Mapping):
            raise BrowseError(
                "backend_invalid_target",
                "The connector must return a selected Playwright target with an identity proof.",
            )
        identity = value.get("selection")
        if not isinstance(identity, Mapping):
            identity = value
        for field_name in _SELECTION_FIELDS:
            if identity.get(field_name) != getattr(selection, field_name):
                raise BrowseError(
                    "backend_invalid_target",
                    "The connector target identity does not exactly match the explicit selection.",
                )
        returned_revision = identity.get("target_revision", value.get("target_revision"))
        if selection.target_revision is not None and selection.target_revision != returned_revision:
            raise BrowseError(
                "stale_target",
                "The selected browser target revision is stale; inspect backend status and select it again.",
            )
        page = value.get("page")
        if page is None:
            raise BrowseError(
                "backend_invalid_target",
                "The selected browser connector did not return a Playwright page.",
            )
        if _page_is_closed(page):
            raise BrowseError(
                "stale_target",
                "The explicitly selected browser tab is already closed or unavailable.",
            )
        returned_family = _browser_family(value.get("browser_family", browser_family))
        if returned_family != _browser_family(browser_family):
            raise BrowseError(
                "backend_invalid_target",
                "The connector returned a browser family different from its declaration.",
            )
        metadata = value.get("metadata")
        if not isinstance(metadata, Mapping):
            metadata = {}
        return cls(
            selection=selection,
            page=page,
            context=value.get("context", value.get("browser_context")),
            browser_family=returned_family,
            target_revision=returned_revision,
            metadata=dict(metadata),
        )


ExistingTarget = PlaywrightTarget


@dataclass(frozen=True)
class BackendDescriptor:
    connector_id: str
    label: str
    protocol: str
    browser: str
    browser_family: str
    state: str
    capabilities: Mapping[str, Mapping[str, Any]]
    limitations: Tuple[str, ...] = ()

    def as_dict(self) -> Dict[str, Any]:
        return {
            "connector_id": bounded_text(self.connector_id, MAX_BACKEND_ID_CHARS),
            "label": bounded_text(self.label, 160),
            "protocol": bounded_text(self.protocol, 80),
            "browser": bounded_text(self.browser, 80),
            "browser_family": bounded_text(self.browser_family, 32),
            "state": bounded_text(self.state, 32),
            "capabilities": {
                bounded_text(name, 64): {
                    "supported": bool(value.get("supported", False)),
                    **(
                        {"reason": bounded_text(value.get("reason"), 256)}
                        if value.get("reason")
                        else {}
                    ),
                }
                for name, value in list(self.capabilities.items())[:32]
                if isinstance(value, Mapping)
            },
            "limitations": [bounded_text(item, 256) for item in self.limitations[:8]],
        }


Selector = Callable[[TargetSelection, ResourceOwner], Any]
Verifier = Callable[[PlaywrightTarget, TargetSelection, ResourceOwner], Any]
Lifecycle = Callable[[PlaywrightTarget, ResourceOwner], Any]


class BrowserConnector:
    """An explicit, injected connector for one browser/session source.

    The selector is the integration boundary.  It must select the exact
    identity passed to it; it must not enumerate or guess a target.  All
    callbacks run on Browse's serialized owner thread once selection begins.
    """

    def __init__(
        self,
        connector_id: str,
        *,
        browser: str = "chromium",
        protocol: str = "playwright",
        selector: Optional[Selector] = None,
        verify_target: Optional[Verifier] = None,
        release: Optional[Lifecycle] = None,
        stop: Optional[Lifecycle] = None,
        label: Optional[str] = None,
        capabilities: Optional[Mapping[str, Any]] = None,
        limitations: Iterable[str] = (),
    ) -> None:
        self.connector_id = _connector_id(connector_id)
        self.browser = bounded_text(browser, 80)
        self.browser_family = _browser_family(browser)
        self.protocol = bounded_text(protocol, 80)
        self.selector = selector
        self._verifier = verify_target
        self._release = release
        self._stop = stop
        self.label = bounded_text(label or connector_id, 160)
        self.limitations = tuple(bounded_text(item, 256) for item in limitations)[:8]
        self.supported = self.browser_family == "chromium"
        reason = (
            "Native %s control is not implemented; no Chromium substitution is performed."
            % bounded_text(self.browser, 80)
            if not self.supported
            else "This existing-session connector does not expose this capability."
        )
        self.capabilities = _capabilities(
            capabilities,
            supported=self.supported,
            reason=reason,
        )
        self._lock = threading.RLock()

    @property
    def descriptor(self) -> BackendDescriptor:
        return BackendDescriptor(
            connector_id=self.connector_id,
            label=self.label,
            protocol=self.protocol,
            browser=self.browser,
            browser_family=self.browser_family,
            state="available" if self.supported else "unsupported",
            capabilities=self.capabilities,
            limitations=self.limitations,
        )

    def describe(self, state: Optional[str] = None) -> Dict[str, Any]:
        descriptor = self.descriptor
        if state is not None:
            descriptor = BackendDescriptor(
                connector_id=descriptor.connector_id,
                label=descriptor.label,
                protocol=descriptor.protocol,
                browser=descriptor.browser,
                browser_family=descriptor.browser_family,
                state=bounded_text(state, 32),
                capabilities=descriptor.capabilities,
                limitations=descriptor.limitations,
            )
        return descriptor.as_dict()

    def select(self, selection: TargetSelection, owner: ResourceOwner) -> PlaywrightTarget:
        self._check_selection(selection)
        if not self.supported:
            raise BrowseError(
                "unsupported_capability",
                "The selected %s backend is unsupported; Chromium is never substituted."
                % bounded_text(self.browser, 80),
            )
        if self.selector is None:
            raise BrowseError(
                "backend_unavailable",
                "This backend requires an explicitly injected connector; ambient browser discovery is disabled.",
            )
        try:
            value = self.selector(selection, owner)
        except BrowseError:
            raise
        except Exception as error:
            raise BrowseError(
                "backend_selection_failed",
                "The explicitly injected browser connector could not select the requested target.",
            ) from error
        target = PlaywrightTarget.from_connector_result(
            value,
            selection,
            self.browser_family,
        )
        self.verify(target, selection, owner)
        return target

    def verify(
        self,
        target: PlaywrightTarget,
        selection: TargetSelection,
        owner: ResourceOwner,
    ) -> bool:
        self._check_selection(selection)
        if target.selection.target_identity != selection.target_identity:
            return False
        if selection.target_revision and target.target_revision != selection.target_revision:
            return False
        if _page_is_closed(target.page):
            return False
        if self._verifier is not None:
            try:
                value = self._verifier(target, selection, owner)
            except BrowseError:
                raise
            except Exception as error:
                raise BrowseError(
                    "backend_verify_failed",
                    "The selected browser target could not be verified safely.",
                ) from error
            if not bool(value):
                raise BrowseError(
                    "stale_target",
                    "The explicitly selected browser target could not be verified; select it again.",
                )
        return True

    def release(self, target: PlaywrightTarget, owner: ResourceOwner) -> None:
        if self._release is None:
            return
        try:
            self._release(target, owner)
        except BrowseError:
            raise
        except Exception as error:
            raise BrowseError(
                "backend_release_failed",
                "The browser connector could not release its selected target safely.",
            ) from error

    def stop(self, target: PlaywrightTarget, owner: ResourceOwner) -> None:
        if self._stop is None:
            raise BrowseError(
                "unsupported_capability",
                "This connector does not expose an explicit stop operation.",
            )
        try:
            self._stop(target, owner)
        except BrowseError:
            raise
        except Exception as error:
            raise BrowseError(
                "backend_stop_failed",
                "The browser connector could not stop its selected target safely.",
            ) from error

    def _check_selection(self, selection: TargetSelection) -> None:
        if selection.connector_id != self.connector_id:
            raise BrowseError(
                "backend_invalid_target",
                "The selected connector does not match the requested connector identity.",
            )
        browser_family = _browser_family(selection.browser_id)
        if browser_family in {"firefox", "safari"}:
            # Opaque browser IDs (for example ``chrome-profile-a``) are
            # accepted; literal native families are never routed to Chromium.
            raise BrowseError(
                "unsupported_capability",
                "The selected browser family is unsupported by this connector.",
            )


class PlaywrightConnector(BrowserConnector):
    """Explicit Playwright Chrome/Edge pairing connector."""

    def __init__(self, connector_id: str, **kwargs: Any) -> None:
        kwargs.setdefault("protocol", "playwright")
        super().__init__(connector_id, **kwargs)


class LunaMaxPlaywrightConnector(PlaywrightConnector):
    """Narrow Luna/max bridge; the Luna connector is supplied by the caller."""

    def __init__(self, connector_id: str, **kwargs: Any) -> None:
        kwargs.setdefault("protocol", "luna-max-playwright")
        super().__init__(connector_id, **kwargs)


class PlaywrightMcpConnector(PlaywrightConnector):
    """Narrow Playwright MCP bridge with explicit injected selection calls."""

    def __init__(self, connector_id: str, **kwargs: Any) -> None:
        kwargs.setdefault("protocol", "playwright-mcp")
        super().__init__(connector_id, **kwargs)


class UnsupportedBrowserConnector(BrowserConnector):
    """Descriptor-only native backend; every operation fails closed."""

    def __init__(self, connector_id: str, *, browser: str, **kwargs: Any) -> None:
        super().__init__(connector_id, browser=browser, **kwargs)


class NativeFirefoxConnector(UnsupportedBrowserConnector):
    def __init__(self, connector_id: str = "native-firefox", **kwargs: Any) -> None:
        kwargs.setdefault("protocol", "native")
        super().__init__(connector_id, browser="firefox", **kwargs)


class NativeSafariConnector(UnsupportedBrowserConnector):
    def __init__(self, connector_id: str = "native-safari", **kwargs: Any) -> None:
        kwargs.setdefault("protocol", "native")
        super().__init__(connector_id, browser="safari", **kwargs)


# Convenient explicit factories for host registration code.
def native_firefox_connector(
    connector_id: str = "native-firefox", **kwargs: Any
) -> NativeFirefoxConnector:
    return NativeFirefoxConnector(connector_id, **kwargs)


def native_safari_connector(
    connector_id: str = "native-safari", **kwargs: Any
) -> NativeSafariConnector:
    return NativeSafariConnector(connector_id, **kwargs)


# Compatibility names for integrations that call these objects adapters.
BrowserAdapter = BrowserConnector
FirefoxConnector = NativeFirefoxConnector
SafariConnector = NativeSafariConnector


class AdapterRegistry:
    """Thread-safe connector registration and exact-target ownership claims."""

    def __init__(self, connectors: Iterable[BrowserConnector] = ()) -> None:
        self._lock = threading.RLock()
        self._connectors: Dict[str, BrowserConnector] = {}
        self._claims: Dict[Tuple[str, str, str, str, str], Tuple[str, str, int]] = {}
        for connector in connectors:
            self.register(connector)

    def register(self, connector: BrowserConnector) -> None:
        if not isinstance(connector, BrowserConnector):
            raise TypeError("connector must be a BrowserConnector")
        with self._lock:
            if connector.connector_id in self._connectors:
                raise ValueError("duplicate browser connector")
            if len(self._connectors) >= MAX_BACKENDS:
                raise ValueError("too many browser connectors")
            self._connectors[connector.connector_id] = connector

    def unregister(self, connector_id: str) -> None:
        connector_id = _connector_id(connector_id)
        with self._lock:
            if any(key[0] == connector_id for key in self._claims):
                raise BrowseError(
                    "backend_in_use",
                    "A selected browser target must be revoked before its connector is removed.",
                )
            self._connectors.pop(connector_id, None)

    def get(self, connector_id: str) -> BrowserConnector:
        connector_id = _connector_id(connector_id)
        with self._lock:
            connector = self._connectors.get(connector_id)
        if connector is None:
            raise BrowseError(
                "backend_missing",
                "The requested browser connector is not explicitly registered.",
            )
        return connector

    def connectors(self) -> List[BrowserConnector]:
        with self._lock:
            return list(self._connectors.values())[:MAX_BACKENDS]

    def claim(self, selection: TargetSelection, owner: ResourceOwner) -> None:
        with self._lock:
            current = self._claims.get(selection.target_identity)
            if current is not None and current != owner.key:
                raise BrowseError(
                    "backend_in_use",
                    "The explicitly selected browser target belongs to another resource owner.",
                )
            self._claims[selection.target_identity] = owner.key

    def release(self, selection: TargetSelection, owner: ResourceOwner, *, force: bool = False) -> None:
        with self._lock:
            current = self._claims.get(selection.target_identity)
            if current is None:
                return
            if not force and current != owner.key:
                raise BrowseError(
                    "owner_mismatch",
                    "The selected browser target belongs to a different resource owner.",
                )
            self._claims.pop(selection.target_identity, None)

    def state(self, connector_id: str, owner: Optional[ResourceOwner] = None) -> str:
        connector = self.get(connector_id)
        if not connector.supported:
            return "unsupported"
        owner_key = owner.key if owner is not None else None
        with self._lock:
            states = [
                claim
                for identity, claim in self._claims.items()
                if identity[0] == connector.connector_id
            ]
        if owner_key is not None and owner_key in states:
            return "selected"
        if states:
            return "in_use"
        return "available"

    def describe(self, owner: Optional[ResourceOwner] = None) -> List[Dict[str, Any]]:
        return [
            connector.describe(self.state(connector.connector_id, owner))
            for connector in self.connectors()
        ]

    def selected(self, owner: ResourceOwner) -> List[TargetSelection]:
        with self._lock:
            identities = [
                identity
                for identity, claim in self._claims.items()
                if claim == owner.key
            ]
        result = []
        for identity in identities:
            result.append(
                TargetSelection(
                    connector_id=identity[0],
                    browser_id=identity[1],
                    session_id=identity[2],
                    window_id=identity[3],
                    tab_id=identity[4],
                )
            )
        return result

    def clear_owner(self, owner: ResourceOwner) -> None:
        with self._lock:
            for identity, claim in list(self._claims.items()):
                if claim == owner.key:
                    self._claims.pop(identity, None)


# Historical integration terminology; retain as an alias without introducing
# another implementation or discovery path.
BackendAdapter = BrowserConnector


__all__ = [
    "AdapterRegistry",
    "BackendAdapter",
    "BackendDescriptor",
    "BackendSelection",
    "BrowserAdapter",
    "BrowserConnector",
    "ExistingTarget",
    "FirefoxConnector",
    "LunaMaxPlaywrightConnector",
    "NativeFirefoxConnector",
    "NativeSafariConnector",
    "PlaywrightConnector",
    "PlaywrightMcpConnector",
    "PlaywrightTarget",
    "SafariConnector",
    "SelectedTarget",
    "TargetSelection",
    "UnsupportedBrowserConnector",
    "native_firefox_connector",
    "native_safari_connector",
]
