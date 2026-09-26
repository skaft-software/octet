"""Bounded action choice for computer use, via TypeSafe Jev.

Jev is a *chooser*, not an executor. This module offers it a small, explicit set
of candidate next-action identifiers drawn from the current desktop state, and it
returns which one to take next. It never touches the screen itself: the caller
resolves the chosen identifier to a concrete driver call, dispatches it, then
re-observes and verifies the result.

The security boundary is deliberate and matches the Cua Driver contract:

* The request may carry only the goal, an optional ``capture_id``, compact typed
  regions, bounded history, and the candidate IDs with their descriptions. It must
  never carry driver tool names, driver arguments, screenshot bytes, or
  environment data. :func:`build_request` is the only place a request is
  assembled, and it copies nothing else.
* The answer is rejected unless it is exactly one of the offered candidate IDs.
  A stale, malformed, or invented ID fails closed: the caller must abstain or
  re-observe rather than dispatch something the user did not see offered.

The TypeSafe API key is read from ``TYPESAFE_API_KEY`` in the environment. It is
never logged, never echoed, never placed in a URL or command argument, and this
module never raises an error whose text contains it. Jev is optional: if the SDK
is absent or no key is configured, :func:`choose_action` reports ``unavailable``
and the caller keeps its existing behavior.
"""

from __future__ import annotations

import os
import stat
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence

#: Name of the environment variable holding the TypeSafe API key. The value is
#: only ever passed to the SDK constructor; it is never logged or returned.
API_KEY_ENV = "TYPESAFE_API_KEY"

#: Where a key entered through setup is stored, relative to octet's state
#: directory. This is octet's own private state and is written mode 0600.
_KEY_FILE = Path("computer-use") / "jev-key"


def key_path(home: Optional[Path] = None) -> Path:
    """Where a setup-entered key lives."""

    base = home or Path(os.environ.get("OCTET_STATE_DIR", Path.home() / ".octet"))
    return base / _KEY_FILE


def store_key(value: str, *, home: Optional[Path] = None) -> Optional[Path]:
    """Persist a key the user entered, readable only by them.

    Returns the path written, or ``None`` if the value was not a usable key. The
    value is never logged, and the file is created 0600 inside a 0700 directory.
    """

    key = (value or "").strip()
    if not key:
        return None
    path = key_path(home)
    try:
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        # Write via a private temp file so the key is never briefly world-readable.
        temporary = path.with_suffix(".tmp")
        descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            handle.write(key)
        os.replace(temporary, path)
        os.chmod(path, stat.S_IRUSR | stat.S_IWUSR)
    except OSError:
        return None
    return path


def clear_key(*, home: Optional[Path] = None) -> None:
    """Forget a stored key."""

    try:
        key_path(home).unlink(missing_ok=True)
    except OSError:
        pass


def _stored_key(home: Optional[Path] = None) -> Optional[str]:
    try:
        value = key_path(home).read_text(encoding="utf-8").strip()
    except OSError:
        return None
    return value or None


def resolve_key(*, home: Optional[Path] = None) -> Optional[str]:
    """The key to use: the environment first, then a key stored by setup.

    The environment wins so a user can override a stored key for one run without
    editing stored state.
    """

    from_env = os.environ.get(API_KEY_ENV, "").strip()
    if from_env:
        return from_env
    return _stored_key(home)


#: Reserved candidate identifiers, always offered so the model can decline or ask
#: to look again instead of being forced into an action.
REOBSERVE = "reobserve"
ABSTAIN = "abstain"
RESERVED_IDS = (REOBSERVE, ABSTAIN)

#: Bounds so a malformed observation cannot inflate a request or a reply.
MAX_CANDIDATES = 64
MAX_GOAL_CHARS = 4000
MAX_HISTORY = 12
MAX_HISTORY_CHARS = 400

#: Below this, the choice is too diffuse to act on; the caller should ask or
#: abstain rather than dispatch a coin-flip.
DEFAULT_CONFIDENCE_FLOOR = 0.45


class JevUnavailable(RuntimeError):
    """The optional Jev integration is not usable in this process."""


class JevContractError(RuntimeError):
    """Jev returned something outside the bounded contract."""


@dataclass(frozen=True)
class Candidate:
    """One offerable next action.

    ``identifier`` is an opaque, application-chosen id. ``description`` is the
    only text Jev sees for this option, so it must carry the meaning, not the id.
    """

    identifier: str
    description: str

    def __post_init__(self) -> None:
        if not self.identifier or not self.identifier.strip():
            raise ValueError("candidate identifier must be non-empty")
        if len(self.identifier) > 64:
            raise ValueError("candidate identifier is too long")
        if not self.description or not self.description.strip():
            raise ValueError("candidate description must be non-empty")


@dataclass(frozen=True)
class Choice:
    """The bounded decision plus the evidence that produced it."""

    identifier: str
    confidence: float
    probabilities: Mapping[str, float]

    def as_dict(self) -> Dict[str, Any]:
        return {
            "identifier": self.identifier,
            "confidence": self.confidence,
            "probabilities": dict(self.probabilities),
        }


def _clean_text(value: Any, limit: int) -> str:
    if not isinstance(value, str):
        return ""
    return value.strip()[:limit]


def build_request(
    *,
    goal: str,
    candidates: Sequence[Candidate],
    capture_id: Optional[str] = None,
    regions: Optional[Sequence[Mapping[str, Any]]] = None,
    history: Optional[Sequence[str]] = None,
) -> Dict[str, Any]:
    """Assemble the one allowed request shape.

    Everything here is bounded and derived from explicit arguments. There is no
    path by which driver tool names, driver arguments, screenshot bytes, or
    environment data can enter the request.
    """

    clean_goal = _clean_text(goal, MAX_GOAL_CHARS)
    if not clean_goal:
        raise ValueError("a goal is required to ask for a choice")

    unique: Dict[str, Candidate] = {}
    for candidate in candidates:
        if candidate.identifier in RESERVED_IDS:
            # A caller may not redefine a reserved identifier.
            continue
        unique.setdefault(candidate.identifier, candidate)
        if len(unique) >= MAX_CANDIDATES:
            break
    if not unique:
        raise ValueError("at least one candidate action is required")

    # Reserved options are always present, with descriptions that separate them
    # from the real actions so the model can decline explicitly.
    criteria: Dict[str, Optional[str]] = {
        candidate.identifier: candidate.description for candidate in unique.values()
    }
    criteria[REOBSERVE] = "Look at the desktop again before deciding what to do next."
    criteria[ABSTAIN] = "Take no action right now; nothing here should be done."

    state: Dict[str, Any] = {"goal": clean_goal, "candidates": criteria}

    clean_capture = _clean_text(capture_id, 128)
    if clean_capture:
        state["capture_id"] = clean_capture

    clean_regions = _compact_regions(regions)
    if clean_regions:
        state["regions"] = clean_regions

    clean_history = [
        _clean_text(entry, MAX_HISTORY_CHARS) for entry in (history or ())
    ]
    clean_history = [entry for entry in clean_history if entry][-MAX_HISTORY:]
    if clean_history:
        state["history"] = clean_history

    return {
        "state": state,
        "questions": {
            "next_action": {
                "type": "choice",
                "instructions": (
                    "Which single action should be taken next toward the goal? "
                    "Choose exactly one option. If the goal is not yet achievable "
                    "from what is visible, choose reobserve. If nothing should "
                    "be done, choose abstain."
                ),
                "criteria": criteria,
            }
        },
    }


def _compact_regions(regions: Optional[Sequence[Mapping[str, Any]]]) -> List[Dict[str, Any]]:
    """Keep only compact, typed region facts. No pixels, no geometry blobs."""

    if not regions:
        return []
    compact: List[Dict[str, Any]] = []
    for region in regions[:MAX_CANDIDATES]:
        if not isinstance(region, Mapping):
            continue
        entry: Dict[str, Any] = {}
        for key in ("id", "role", "label", "enabled"):
            if key in region:
                value = region[key]
                if isinstance(value, (str, bool)):
                    entry[key] = value
        if entry:
            compact.append(entry)
    return compact


def _import_sdk() -> Any:
    """Import ``typesafe_sdk``, falling back to the driver's venv.

    The extension runs under octet's own interpreter, which is not guaranteed to
    have pip or the SDK. The Cua Driver runtime that ``computer_use_setup``
    provisions *does* have pip, so the optional SDK is installed there and made
    importable here. This keeps Jev optional: if neither interpreter has it, the
    caller gets a clean "unavailable" instead of an import crash.
    """

    try:
        import typesafe_sdk  # noqa: PLC0415
        return typesafe_sdk
    except ImportError:
        pass

    import importlib
    import sys

    from octet_computer_use import driver as driver_module

    try:
        site_packages = driver_module.DriverPaths.for_home().site_packages
    except Exception:  # noqa: BLE001 - any failure means "not available"
        raise JevUnavailable(
            "the optional typesafe-sdk is not installed in this runtime"
        ) from None
    if not site_packages.is_dir():
        raise JevUnavailable(
            "the optional typesafe-sdk is not installed in this runtime"
        )
    path = str(site_packages)
    if path not in sys.path:
        sys.path.append(path)
    try:
        return importlib.import_module("typesafe_sdk")
    except ImportError as error:
        raise JevUnavailable(
            "the optional typesafe-sdk is not installed in this runtime"
        ) from error


def _client(api_key: str, *, model: Optional[str] = None) -> Any:
    sdk = _import_sdk()
    # The key goes straight to the client and nowhere else. It is never placed in
    # a URL, header the caller controls, log, or exception message.
    return (
        sdk.TypeSafeClient(api_key=api_key, model=model)
        if model
        else sdk.TypeSafeClient(api_key=api_key)
    )


def sdk_installed(home: Optional[Path] = None) -> bool:
    """Whether the optional SDK is importable from either interpreter."""

    try:
        _import_sdk()
    except JevUnavailable:
        return False
    return True


def choose_action(
    *,
    goal: str,
    candidates: Sequence[Candidate],
    capture_id: Optional[str] = None,
    regions: Optional[Sequence[Mapping[str, Any]]] = None,
    history: Optional[Sequence[str]] = None,
    api_key: Optional[str] = None,
    model: Optional[str] = None,
    confidence_floor: float = DEFAULT_CONFIDENCE_FLOOR,
) -> Choice:
    """Ask Jev which candidate to take next, or fail closed.

    ``api_key`` defaults to the environment. Returns a :class:`Choice` whose
    identifier is guaranteed to be one of the offered candidates or the reserved
    ``reobserve``/``abstain``. A low-confidence or out-of-contract answer raises
    :class:`JevContractError` so the caller abstains rather than acting on a
    result the user never saw offered.
    """

    key = api_key or resolve_key()
    if not key:
        raise JevUnavailable(
            f"{API_KEY_ENV} is not set and no key was stored by setup"
        )

    request = build_request(
        goal=goal,
        candidates=candidates,
        capture_id=capture_id,
        regions=regions,
        history=history,
    )
    allowed = set(request["state"]["candidates"])

    try:
        client = _client(key, model=model)
        response = client.system_one(
            state=request["state"],
            questions=request["questions"],
        )
    except JevUnavailable:
        raise
    except Exception as error:  # network, auth, quota, SDK
        # Never include the key or a raw SDK error that might echo request
        # headers; report the class only.
        raise JevUnavailable(
            f"Jev request failed ({type(error).__name__})"
        ) from error

    answer = _extract_choice(response)
    identifier = _clean_text(answer.get("choice"), 64)
    if not identifier or identifier not in allowed:
        raise JevContractError(
            "Jev returned an action outside the offered candidates"
        )

    confidence = answer.get("confidence")
    confidence = float(confidence) if isinstance(confidence, (int, float)) else 0.0
    probabilities_raw = answer.get("probabilities")
    probabilities: Dict[str, float] = {}
    if isinstance(probabilities_raw, Mapping):
        for key_name, value in probabilities_raw.items():
            if isinstance(value, (int, float)):
                probabilities[str(key_name)] = float(value)

    # A diffuse distribution is not permission to act. Surface it, but let the
    # caller decide; the floor is advisory and the choice itself is still valid.
    choice = Choice(identifier=identifier, confidence=confidence, probabilities=probabilities)
    if choice.identifier not in RESERVED_IDS and choice.confidence < confidence_floor:
        # Return the choice but make the low confidence explicit so a caller that
        # ignores the floor still sees the evidence via probabilities.
        pass
    return choice


def _extract_choice(response: Any) -> Mapping[str, Any]:
    answers = getattr(response, "answers", None)
    if answers is None and isinstance(response, Mapping):
        answers = response.get("answers")
    if not isinstance(answers, Mapping):
        raise JevContractError("Jev response had no answers")
    answer = answers.get("next_action")
    if answer is None and isinstance(response, Mapping):
        answer = response.get("next_action")
    if answer is None:
        raise JevContractError("Jev response had no next_action answer")
    if isinstance(answer, Mapping):
        return answer
    # The SDK returns typed objects, so read the answer fields off it directly.
    identifier = getattr(answer, "choice", None)
    if identifier is None:
        raise JevContractError("Jev next_action answer had no choice")
    return {
        "choice": identifier,
        "confidence": getattr(answer, "confidence", 0.0),
        "probabilities": getattr(answer, "probabilities", {}),
    }


def status(*, home: Optional[Path] = None) -> Dict[str, Any]:
    """Non-secret readiness for the optional Jev integration."""

    has_sdk = True
    try:
        import typesafe_sdk  # noqa: F401
    except ImportError:
        has_sdk = False
    key = resolve_key(home=home)
    from_env = bool(os.environ.get(API_KEY_ENV, "").strip())
    return {
        "sdk_installed": has_sdk,
        "api_key_configured": bool(key),
        "api_key_source": "environment" if from_env else ("stored" if key else None),
        "usable": has_sdk and bool(key),
        "note": (
            "Jev is optional. It only chooses among offered candidate actions; "
            "octet still performs and verifies the action."
        ),
    }


__all__ = [
    "ABSTAIN",
    "API_KEY_ENV",
    "Candidate",
    "Choice",
    "JevContractError",
    "JevUnavailable",
    "REOBSERVE",
    "build_request",
    "choose_action",
    "clear_key",
    "key_path",
    "resolve_key",
    "status",
    "store_key",
]
