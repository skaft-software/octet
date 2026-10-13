"""A07: real SDK progress plus a bounded, explicit BlobRef model projection."""
from dataclasses import dataclass
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from octet_extension import BlobRef, Extension

LOG = Path(sys.argv[1])
PAYLOAD = b"A07-private-bulk-payload" * 16384
ext = Extension(api_version="0.4", max_concurrent_requests=1,
                supported_features=["request_cancellation", "content_parts", "request_progress"])


def log(event):
    with LOG.open("a", encoding="utf-8") as stream:
        stream.write(json.dumps({"pid": os.getpid(), "event": event}) + "\n")


@dataclass
class Input:
    pass


@dataclass
class Published:
    data: BlobRef


def summary(result):
    return "A07 published descriptor: " + json.dumps(result.data.to_wire(), separators=(",", ":"))


@ext.typed_tool(name="typed_progress_descriptor", description="Publish bytes with ephemeral progress",
                summary=summary)
def publish(args: Input) -> Published:
    log("entered")
    ext.cancellation.raise_if_cancelled()
    ext.progress(message="a07_ephemeral_step_one", current=1, total=2, unit="steps")
    log("progress_one")
    result = Published(ext.bulk.publish_bytes(PAYLOAD))
    ext.cancellation.raise_if_cancelled()
    ext.progress(message="a07_ephemeral_step_two", current=2, total=2, unit="steps")
    log("progress_two")
    log("returned")
    return result


@ext.on_shutdown
def shutdown(params):
    log("shutdown")


if __name__ == "__main__":
    log("started")
    ext.run()
