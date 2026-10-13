"""Resource-aware SDK executable; host tests supply a private workspace/log."""
from dataclasses import dataclass
import json
import os
from pathlib import Path
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from octet_extension import Extension, Resource, RpcError, TypedResult

LOG = Path(sys.argv[1])
ext = Extension(api_version="0.4", max_concurrent_requests=1)


def log(event, **details):
    with LOG.open("a", encoding="utf-8") as stream:
        stream.write(json.dumps({"pid": os.getpid(), "event": event, **details}) + "\n")


def dispose(counter):
    try:
        _ = counter.reference.value
    except RpcError:
        log("disposed", n=counter.n, invalidated=True)
    else:
        raise AssertionError("destructor ran before reference invalidation")
    if counter.fail_cleanup:
        raise RuntimeError("fixture disposer failure")


@ext.resource_type("example.Counter.v1", dispose=dispose)
class Counter:
    def __init__(self, n, fail_cleanup=False):
        self.n, self.fail_cleanup = n, fail_cleanup
        self.reference = None


@dataclass
class Create:
    n: int = 0
    fail_cleanup: bool = False


@dataclass
class Created:
    counter: Resource[Counter]


@dataclass
class Increment:
    counter: Resource[Counter]
    delta: int = 1


def export(args):
    counter = Counter(args.n, args.fail_cleanup)
    counter.reference = ext.export(counter)
    return Created(counter.reference)


@ext.typed_tool(name="create", description="Create a native counter", summary=lambda result: "Created counter.")
def create(args: Create) -> Created:
    log("create")
    return export(args)


@ext.typed_tool(name="increment", description="Mutate the same native counter", receiver="/counter", summary=lambda result: "Incremented counter.")
def increment(args: Increment) -> int:
    log("increment")
    args.counter.value.n += args.delta
    return args.counter.value.n


@ext.typed_tool(name="invalid_output", description="Refuse a provisional invalid result", summary=lambda result: "Not admitted.")
def invalid_output(args: Create) -> Created:
    log("invalid_output")
    export(args)
    return "invalid"


@ext.typed_tool(name="failed_parent", description="Retire provisional state on domain failure")
def failed_parent(args: Create) -> TypedResult[Created]:
    log("failed_parent")
    export(args)
    return TypedResult(None, "Domain failure.", is_error=True)


@ext.typed_tool(name="hold", description="Hold native execution behind an explicit barrier", receiver="/counter", summary=lambda result: "Held counter.")
def hold(args: Increment) -> int:
    log("entered")
    while not (LOG.parent / "allow_terminal").exists():
        # Deliberately defer cooperation to prove cancellation alone is not a
        # native execution fence. The host still owns its finite grace deadline.
        time.sleep(0.005)
    log("settled", n=args.counter.value.n)
    ext.cancellation.raise_if_cancelled()
    return args.counter.value.n


@ext.on_shutdown
def shutdown(params):
    log("shutdown")


if __name__ == "__main__":
    log("started")
    ext.run()
