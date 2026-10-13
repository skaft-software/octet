"""Bounded actual-process bulk helper fixture; never sends payload bytes in JSON."""
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from octet_extension import BlobRef, Extension, TypedResult

LOG = Path(sys.argv[1])
ext = Extension(api_version="0.4", max_concurrent_requests=1)


def log(event, **details):
    with LOG.open("a", encoding="utf-8") as stream:
        stream.write(json.dumps({"pid": os.getpid(), "event": event, **details}) + "\n")


@dataclass
class Publish:
    length: int = 512 * 1024


@dataclass
class Published:
    data: BlobRef


@dataclass
class Read:
    data: BlobRef


@dataclass
class Measured:
    bytes: int
    sha256: str


def publish(args):
    if not 0 <= args.length <= 1024 * 1024:
        raise ValueError("fixture payload bound")
    data = (bytes(range(256)) * ((args.length + 255) // 256))[:args.length]
    result = ext.bulk.publish_bytes(data)
    log("committed", reference=result.to_wire())
    return Published(result)


@ext.typed_tool(name="publish", description="Publish binary bulk", summary=lambda result: "Published bytes.")
def publish_tool(args: Publish) -> Published:
    return publish(args)


@ext.typed_tool(name="measure", description="Measure bounded binary data", summary=lambda result: "Measured bytes.")
def measure(args: Read) -> Measured:
    with ext.bulk.read(args.data, max_bytes=1024 * 1024) as stream:
        data = stream.read()
    log("read_closed", closed=stream.closed)
    return Measured(len(data), hashlib.sha256(data).hexdigest())


@ext.typed_tool(name="invalid_output", description="Retire provisional blob on invalid output", summary=lambda result: "Not admitted.")
def invalid_output(args: Publish) -> Published:
    publish(args)
    return "invalid"


@ext.typed_tool(name="failed_parent", description="Retire provisional blob on domain failure")
def failed_parent(args: Publish) -> TypedResult[Published]:
    publish(args)
    return TypedResult(None, "Domain failure.", is_error=True)


@ext.typed_tool(name="cancel_publish", description="Cancel after provisional commit", summary=lambda result: "Never admitted.")
def cancel_publish(args: Publish) -> Published:
    result = publish(args)
    log("entered")
    try:
        ext.cancellation.wait(3)
        ext.cancellation.raise_if_cancelled()
        raise RuntimeError("fixture cancellation missing")
    finally:
        log("cancelled")


@ext.typed_tool(name="cancel_read", description="Release bounded reader on cancellation", summary=lambda result: "Never admitted.")
def cancel_read(args: Read) -> Measured:
    stream = None
    try:
        with ext.bulk.read(args.data, max_bytes=1024 * 1024) as stream:
            log("entered")
            ext.cancellation.wait(3)
            ext.cancellation.raise_if_cancelled()
            raise RuntimeError("fixture cancellation missing")
    finally:
        log("cancel_read_closed", closed=stream is not None and stream.closed)


@ext.on_shutdown
def shutdown(params):
    log("shutdown")


if __name__ == "__main__":
    log("started")
    ext.run()
