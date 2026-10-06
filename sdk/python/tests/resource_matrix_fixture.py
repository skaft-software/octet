"""Additional real SDK resource transactions; inherited counter is native state."""
from dataclasses import dataclass
import time

from resource_fixture import Counter, Create, Created, LOG, Resource, RpcError, ext, export, log

# Two lanes allow a held provisional creator and an independent call. No pins or
# authority are simulated here: the production host still admits every call.
ext.max_concurrent_requests = 2


def capture(args):
    result = export(args)
    log("registered", reference=result.counter.to_wire(), n=args.n)
    return result


@ext.typed_tool(name="create_held", description="Hold provisional publication", summary=lambda result: "Created held counter.")
def create_held(args: Create) -> Created:
    result = capture(args)
    log("output_ready")
    observed = False
    while not (LOG.parent / "allow_terminal").exists():
        if ext.cancellation.cancelled and not observed:
            log("cancel_observed")
            observed = True
        time.sleep(0.002)  # File barrier, not a timing-based race oracle.
    log("terminal_ready", cancelled=ext.cancellation.cancelled)
    return result  # Runtime owns cancellation-vs-success terminal selection.


@dataclass
class Pair:
    first: Resource[Counter]
    second: Resource[Counter]
    valid: bool


@ext.typed_tool(name="invalid_pair", description="Refuse both provisional outputs", summary=lambda result: "Never admitted.")
def invalid_pair(args: Create) -> Pair:
    first = capture(args).counter
    second = capture(Create(args.n + 1)).counter
    return Pair(first, second, "not-a-boolean")


@dataclass
class Quota:
    count: int = 33


@ext.typed_tool(name="quota", description="Exercise parent registration bound", summary=lambda result: "Never published.")
def quota(args: Quota) -> int:
    if not 1 <= args.count <= 33:
        raise ValueError("bounded fixture count")
    for n in range(args.count):
        try:
            capture(Create(n))
        except RpcError as error:
            log("registration_refused", index=n, code=error.code, message=error.message, data=error.data)
            raise
    return args.count  # Unexported references must retire even on success.


@ext.typed_tool(name="invalid_captured", description="Capture then reject output", summary=lambda result: "Never admitted.")
def invalid_captured(args: Create) -> Created:
    capture(args)
    return "invalid"


@ext.typed_tool(name="failed_captured", description="Capture then fail parent", summary=lambda result: "Never admitted.")
def failed_captured(args: Create) -> Created:
    capture(args)
    raise RpcError(-32000, "fixture domain failure")


if __name__ == "__main__":
    if (LOG.parent / "reject_candidate").exists():
        log("candidate_rejected")
        raise SystemExit(7)
    log("started")
    ext.run()
