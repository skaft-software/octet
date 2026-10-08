#!/usr/bin/env python3
from pathlib import Path
from octet_extension import Extension

ext = Extension(api_version="0.4", max_concurrent_requests=1)
PATH_ARG = {"type": "object", "properties": {"path": {"type": "string"}},
            "required": ["path"], "additionalProperties": False}

def words(path):
    ext.cancellation.raise_if_cancelled()
    return f"{path}: {len(Path(path).read_text(encoding='utf-8').split())} words"

@ext.tool(name="word_count", description="Count the words in a file", parameters=PATH_ARG)
def word_count(args):
    return words(args["path"])

@ext.command(name="wordcount", description="Count the words in a file", usage="/wordcount PATH")
def wordcount(arguments):
    return words(arguments[0]) if arguments else "usage: /wordcount PATH"
ext.run()
