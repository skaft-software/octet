#!/usr/bin/env python3
"""API 0.4 bitmap compaction strategy; no provider calls or model tools."""

import json
import os
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(os.environ.get("OCTET_EXTENSION_DIR", Path(__file__).resolve().parent)).resolve()
# A source-checkout convenience; an installed copy uses its installed SDK.
source_sdk = ROOT.parents[1] / "sdk" / "python"
if source_sdk.is_dir():
    sys.path.insert(0, str(source_sdk))
from octet_extension import Extension  # noqa: E402

ext = Extension(api_version="0.4", max_concurrent_requests=1,
                supported_features=("request_cancellation", "content_parts",
                                    "compaction_strategy"))
RENDERER = ROOT / "renderer" / "target" / "release" / "octet-snap-renderer"


@ext.hook("compaction_strategy")
def compact(payload, _context):
    model_id = payload.get("model_id") if isinstance(payload, dict) else None
    text = payload.get("text") if isinstance(payload, dict) else None
    if not isinstance(model_id, str) or len(model_id.encode()) > 256:
        raise ValueError("invalid model ID")
    if not isinstance(text, str) or not text or len(text) > 2048 or len(text.encode()) > 16 * 1024:
        raise ValueError("invalid source slice")
    if not RENDERER.is_file():
        raise RuntimeError("renderer missing; build renderer/Cargo.toml first")
    ext.cancellation.raise_if_cancelled()
    child = subprocess.Popen([str(RENDERER)], stdin=subprocess.PIPE,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    deadline = time.monotonic() + 25
    try:
        request = json.dumps({"model_id": model_id, "text": text}).encode()
        while True:
            try:
                output, errors = child.communicate(input=request, timeout=0.1)
                break
            except subprocess.TimeoutExpired:
                request = None
                ext.cancellation.raise_if_cancelled()
                if time.monotonic() > deadline:
                    raise TimeoutError("renderer timed out")
        ext.cancellation.raise_if_cancelled()
        if child.returncode != 0 or len(output) > 800 * 1024:
            raise RuntimeError("renderer failed or exceeded output bounds")
        response = json.loads(output)
        return {"compaction_frames": response["compaction_frames"]}
    finally:
        if child.poll() is None:
            child.kill()
        child.wait()


if __name__ == "__main__":
    ext.run()
