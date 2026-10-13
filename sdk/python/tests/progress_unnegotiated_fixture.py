"""A07 real SDK: ordinary API 0.4 optional-feature negotiation without progress."""
from dataclasses import dataclass
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from octet_extension import Extension, RpcError

LOG = Path(sys.argv[1])
ext = Extension(api_version="0.4", max_concurrent_requests=1,
                supported_features=["request_cancellation", "content_parts"])


def log(event, **details):
    with LOG.open("a", encoding="utf-8") as stream:
        stream.write(json.dumps({"pid": os.getpid(), "event": event, **details}) + "\n")


@dataclass
class Input:
    value: int


@ext.typed_tool(name="unnegotiated_progress", description="Attempt unsupported progress",
                summary=lambda result: "Unexpected progress success.")
def attempt(args: Input) -> int:
    log("attempt", negotiated_features=sorted(ext.negotiated_features),
        host_offered_progress="request_progress" in ext.initialization["protocol"]["optional_features"])
    try:
        ext.progress(message="a07_unnegotiated_must_not_emit", current=1, total=1)
    except RpcError as error:
        log("refused", error=error.error_object())
        raise
    log("sent")
    return args.value


@ext.typed_tool(name="healthy", description="Healthy typed follow-up",
                summary=lambda result: "Healthy typed result.")
def healthy(args: Input) -> int:
    log("healthy")
    return args.value + 1


@ext.on_shutdown
def shutdown(params):
    log("shutdown")


if __name__ == "__main__":
    log("started")
    ext.run()
