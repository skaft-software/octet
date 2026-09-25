"""Reasoning identifier validation and retained legacy effort helpers.

Spawn requests validate identifiers here, then forward them unchanged. The host
alone resolves configured models and normalizes reasoning using its model
metadata; discovery exposes its supported choices. Caller capability hints are
legacy input, not execution authority.

The standalone effort helpers retain their historical ladder/clamp behavior for
legacy consumers and tests. They are not used to resolve worker settings and do
not model binary or always-on reasoning controls.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, List, Optional, Sequence, Tuple

from .model import SubagentError

INHERIT = "inherit"

# ReasoningEffort declaration order (octet-ai `types.rs`).
EFFORT_LADDER: Tuple[str, ...] = (
    "minimal",
    "low",
    "medium",
    "high",
    "xhigh",
    "max",
    "ultra",
)
# Legacy effort-helper choices; on/off control modes are normalized by the host.
EFFORT_LEVELS: Tuple[str, ...] = ("off",) + EFFORT_LADDER
INHERITABLE_LEVELS: Tuple[str, ...] = (INHERIT, "on") + EFFORT_LEVELS

# wire defaults: `default_min_effort` / `default_max_effort`.
DEFAULT_FLOOR = "minimal"
DEFAULT_CEILING = "high"


@dataclass(frozen=True)
class ReasoningCapability:
    """Legacy caller hint, never authoritative for worker execution."""

    ceiling: str = DEFAULT_CEILING
    floor: str = DEFAULT_FLOOR
    # `model_supports_ultra`: an effort-controlled model that advertises Ultra.
    ultra_advertised: bool = False

    @classmethod
    def parse(cls, value: Any) -> "ReasoningCapability":
        if value is None:
            return cls()
        if not isinstance(value, dict):
            raise SubagentError(
                "reasoning_capability must be an object with ceiling/floor/ultra",
                code="unsupported_reasoning",
            )
        unknown = set(value) - {"ceiling", "floor", "ultra"}
        if unknown:
            raise SubagentError(
                "unknown reasoning_capability fields: %s" % ", ".join(sorted(unknown)),
                code="unsupported_reasoning",
            )
        return cls(
            ceiling=_effort(value.get("ceiling", DEFAULT_CEILING), "reasoning_capability.ceiling"),
            floor=_effort(value.get("floor", DEFAULT_FLOOR), "reasoning_capability.floor"),
            ultra_advertised=_boolean(value.get("ultra", False), "reasoning_capability.ultra"),
        )

    def order(self) -> Tuple[int, int]:
        return (
            EFFORT_LADDER.index(self.floor),
            EFFORT_LADDER.index(self.ceiling),
        )


def _effort(value: Any, name: str) -> str:
    if not isinstance(value, str) or value not in EFFORT_LADDER:
        raise SubagentError(
            "%s must be one of: %s" % (name, ", ".join(EFFORT_LADDER)),
            code="unsupported_reasoning",
        )
    return value


def _boolean(value: Any, name: str) -> bool:
    if not isinstance(value, bool):
        raise SubagentError("%s must be a boolean" % name, code="unsupported_reasoning")
    return value


def parse_level(value: Any, name: str = "reasoning") -> str:
    """Validate a reasoning identifier without applying model policy."""
    if value is None:
        return INHERIT
    if not isinstance(value, str) or value not in INHERITABLE_LEVELS:
        raise SubagentError(
            "%s must be one of: %s" % (name, ", ".join(INHERITABLE_LEVELS)),
            code="unsupported_reasoning",
        )
    return value


def choices(capability: ReasoningCapability) -> List[str]:
    """Mirror `ReasoningCapability::choices` for an effort-controlled model."""
    low, high = capability.order()
    if low > high:
        return ["off"]
    values = ["off"]
    values.extend(
        level for index, level in enumerate(EFFORT_LADDER) if low <= index <= high
    )
    return values


def supports(capability: ReasoningCapability, level: str) -> bool:
    return level in choices(capability)


def thinking_to_reasoning(level: str, capability: ReasoningCapability) -> str:
    """Mirror `thinking_to_reasoning` for efforts, including its fallback order."""
    level = parse_level(level)
    if level == INHERIT:
        return INHERIT
    available = choices(capability)
    if level in available:
        return level
    if level in EFFORT_LADDER:
        index = EFFORT_LADDER.index(level)
        lower = [
            candidate
            for candidate in available
            if candidate in EFFORT_LADDER and EFFORT_LADDER.index(candidate) <= index
        ]
        if lower:
            # highest supported effort at or below the request
            return max(lower, key=EFFORT_LADDER.index)
        non_off = [candidate for candidate in available if candidate != "off"]
        if non_off:
            return non_off[0]
        return available[0]
    # `off` on an always-reasoning model: the product falls back to the model's
    # default selection, i.e. the lowest enabled non-Off choice.
    non_off = [candidate for candidate in available if candidate != "off"]
    if non_off:
        return non_off[0]
    return available[0]


def supported_levels(
    capability: ReasoningCapability, *, subagents_available: bool = True
) -> List[str]:
    """Mirror `supported_levels_with_subagents` over the effort ladder."""
    levels = [
        level
        for level in choices(capability)
        if level != "ultra" or (capability.ultra_advertised and subagents_available)
    ]
    return levels


def clamp_and_describe(
    level: str, capability: Optional[ReasoningCapability]
) -> Tuple[str, Optional[str]]:
    """Return the effective level and an explicit note when it was clamped.

    `inherit` always stays `inherit`: the parent session's already-normalized
    selection is the child's, and the host owns that normalization.
    """
    requested = parse_level(level)
    if requested == INHERIT or capability is None:
        return requested, None
    effective = thinking_to_reasoning(requested, capability)
    if effective == requested:
        return effective, None
    return (
        effective,
        "requested %s clamped to %s by the model ceiling %s"
        % (requested, effective, capability.ceiling),
    )


def describe(capability: Optional[ReasoningCapability]) -> str:
    if capability is None:
        return "inherited"
    return "ceiling %s" % capability.ceiling


def levels_hint(capability: Optional[ReasoningCapability]) -> Sequence[str]:
    return supported_levels(capability) if capability is not None else EFFORT_LEVELS
