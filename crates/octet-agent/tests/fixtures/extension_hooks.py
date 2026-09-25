"""Retained API 0.2 hook fixture, not an API 0.3 authoring example."""
import json
from pathlib import Path
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "sdk" / "python"))
from octet_extension import Extension, persistence_metadata, post_mutation_rescan

extension = Extension(api_version="0.2")
log = Path(sys.argv[1])
mode = sys.argv[2]


def record(hook, payload, context):
    with log.open("a") as output:
        output.write(json.dumps({"hook": hook, "payload": payload, "context": context}) + "\n")


@extension.tool(name="decorate", description="Emit ephemeral bounded progress")
def decorate(arguments):
    extension.progress_decoration("ephemeral-label", "ephemeral-detail")
    extension.progress_decoration("é" * 128, "é" * 2048)
    return "immutable-final-output"


@extension.hook("before_persistence")
def before_persistence(payload, context):
    record("before_persistence", payload, context)
    return {"persistence_metadata": persistence_metadata({"annotation": "private-marker"})}


@extension.hook("post_mutation")
def post_mutation(payload, context):
    record("post_mutation", payload, context)
    if mode == "timeout":
        time.sleep(1)
    if mode == "outside":
        resources = ["resource:foreign"]
    elif mode == "malformed":
        resources = ["/private/secret"]
    elif mode == "empty":
        return {"post_mutation": {"action": "no_rescan"}}
    else:
        resources = payload["affected_resources"]
    return {"post_mutation": post_mutation_rescan(resources)}


extension.run()
