"""Typed, non-authoritative cache-refresh advice for extension API 0.4."""

from __future__ import annotations

from typing import Any, Callable, Literal, Optional, TypedDict, Union


CacheWarmingAction = Literal["warm", "stop"]


class CacheWarmingDecision(TypedDict):
    """Host economics for one due refresh; no conversation content is exposed."""

    phase: Literal["streaming", "idle"]
    warm_cost_microdollars: int
    miss_cost_microdollars: int
    continuation_probability: float
    expected_savings_microdollars: int
    economics_available: bool
    action: CacheWarmingAction


class CacheWarmingDecisionPayload(TypedDict):
    """Hook payload; the owner fence is in the separate execution context."""

    decision: CacheWarmingDecision
    model: str


class CacheWarmingDecisionResult(TypedDict):
    """A single opinion; null leaves the preceding host/hook action unchanged."""

    cache_warming_decision: Optional[CacheWarmingAction]


CacheWarmingDecisionHandler = Union[
    Callable[[CacheWarmingDecisionPayload], Optional[CacheWarmingAction]],
    Callable[[CacheWarmingDecisionPayload, dict[str, Any]], Optional[CacheWarmingAction]],
]


def cache_warming_decision(action: Optional[CacheWarmingAction]) -> CacheWarmingDecisionResult:
    """Build a hook result without changing budgets, deadlines, or eligibility."""

    if action is not None and action not in ("warm", "stop"):
        raise ValueError("cache warming action must be warm, stop, or None")
    return {"cache_warming_decision": action}
