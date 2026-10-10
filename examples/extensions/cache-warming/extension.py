#!/usr/bin/env python3
"""Apply a stricter local spending preference to host-owned cache refreshes."""

from typing import Optional

from octet_extension import CacheWarmingAction, CacheWarmingDecisionPayload, Extension


ext = Extension(api_version="0.4", max_concurrent_requests=1)
MAX_PREFERRED_WARM_MICRODOLLARS = 5_000


@ext.cache_warming_decision
def advise(payload: CacheWarmingDecisionPayload) -> Optional[CacheWarmingAction]:
    ext.cancellation.raise_if_cancelled()
    decision = payload["decision"]
    if decision["warm_cost_microdollars"] > MAX_PREFERRED_WARM_MICRODOLLARS:
        return "stop"
    # No opinion preserves the host's economics (or an earlier hook's action).
    return None


if __name__ == "__main__":
    ext.run()
