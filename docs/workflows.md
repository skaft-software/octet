# Basic usage

[Documentation](README.md) · [Getting started](getting-started.md)

## Start a session

From your repository, with a [source-built
binary](installation.md#build-from-a-checkout) and a [connected
model](providers.md):

```sh
octet --safe-mode
```

```text
Explain how this project handles authentication. Show me the relevant files.
```

Safe mode asks before file changes and every shell call. It isn't a sandbox, so
use OS isolation for untrusted code. Without it, octet has full access. See
[Tools and permissions](tools.md).

## Make a change

```text
Fix the parser's handling of empty input. Add a regression test and run it.
Do not commit the change.
```

State the goal, the limits and what "done" looks like. Review each action before
you approve it, then read the diff and run the tests yourself. For tasks you
repeat, use [prompt templates](instructions.md#prompt-templates).
`/answer [instruction]` makes octet stop using tools at the next safe point and
answer from what it has found ([commands](commands.md)).

## Resume work

```sh
octet --continue
```

This reopens the latest session in the current workspace. Use
`octet --resume SESSION_ID` for a specific one. In a session, `/fork` branches
from an earlier message and `/clone` copies the current state. See
[Sessions](sessions.md).

## Images and audio

Paste or drop a file path into the terminal, or pick the file with `@`
completion. Wait for the image or audio chip before you send, because a typed
path isn't an attachment. Native audio is WAV or MP3, only on compatible OpenAI
Chat routes, and the graphical composer doesn't take audio. Formats, limits and
privacy are in [Images and audio](media.md).

## Subagents

With the [subagents package](../extensions/octet-subagents/README.md) enabled
and trusted:

```text
Have one worker trace the parser and another inspect its tests.
Give both only read and search tools. Summarize their findings; do not edit.
```

Unless you narrow them, workers get the same tools as the main session. They run
one level deep, with at most 8 active and 32 kept in the record. They inherit
your permissions and cost and token limits, and you can cancel them and read
their transcripts. They share your working directory, which isn't isolation.
Open `/subagents`, including during a run, to see each worker's phase, tool
calls, tokens, spend and transcript. Use `/subagents stop <name-or-id|all>` to
request interruption; `/extensions` only manages extension enablement and
configuration. See [Subagents](../extensions/octet-subagents/README.md).

Extensions need full-access mode, and installing, enabling and trusting are
separate steps. Catalog setup works once the matching release is published
([Optional packages](installation.md#optional-packages)). The bundle declares
API 0.4, and exact host-version pins still apply.

## Extend or embed

- Add instructions, prompts and skills: [instructions](instructions.md) and
  [resource discovery](resources.md).
- Add tools in any language: [extensions](extensions.md) and the [API 0.4
  reference](extensions/API-0.4-REFERENCE.md). Start from the current Python
  process recipe, and keep exact-version examples and conformance tests as they
  are rather than relabeling them.
- Use the [browser](../extensions/octet-browse/README.md), [web
  search](../extensions/octet-web-search/README.md) or
  [MCP](../extensions/octet-mcp/README.md) packages.
- Bring over a Pi setup: [Pi import and restore](pi-migration.md).
- Embed octet with [native host protocol 1](sdk.md), or try the optional
  [graphical Serve interface](experimental/octet-serve/README.md).
