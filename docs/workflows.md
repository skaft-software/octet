# Basic usage

[Documentation](README.md) · [Getting started](getting-started.md)

## Start a session

From your repository, using your [source-built binary](installation.md#build-from-a-checkout)
and a [configured provider](providers.md):

```sh
octet --safe-mode
```

```text
Explain how this project handles authentication. Show me the relevant files.
```

Safe mode asks before file changes and every shell call. It is not a sandbox;
use OS isolation for untrusted code. Full access is the default without it.
[Tools and permissions](tools.md).

## Make a change

```text
Fix the parser's handling of empty input. Add a regression test and run it.
Do not commit the change.
```

Review proposed actions before approving them, then inspect the resulting diff
and tests yourself. Name the goal, constraints, and expected result explicitly;
use [prompt templates](instructions.md#prompt-templates) for repeated tasks.
`/answer [instruction]` asks for an answer from existing evidence at the next safe
boundary instead of further tools. [Run controls](commands.md).

## Resume work

```sh
octet --continue
```

This reopens the latest current-workspace session. Use
`octet --resume SESSION_ID` for a specific session, `/fork` for an earlier user
message, or `/clone` for the current head. [Sessions](sessions.md).

## Images and audio

Explicitly paste/drop a path through the terminal paste mechanism, or select it
with `@` completion. Check the image/audio chip before submitting; typed paths
alone are not attachments. Native audio is WAV/MP3 only on compatible OpenAI Chat
routes; the graphical composer does not accept audio. See the
[audio/image recipe, formats, limits, and privacy rules](media.md).

## Subagents

With the [subagents package](../extensions/octet-subagents/README.md) enabled and
independently trusted:

```text
Have one worker trace the parser and another inspect its tests.
Give both only read and search tools. Summarize their findings; do not edit.
```

Workers otherwise inherit the parent's standard read/search/edit/write/bash
scope. The host enforces depth one, at most eight active children and thirty-two
retained records, inherited policy and cost/token limits, cancellation, and
owner-authorized read-only transcripts. A shared cwd is not isolation. Open
`/subagents` to inspect phase, tool calls, tokens, spend, and transcripts.
Cleanup retains bounded terminal diagnostics/roster when a live record disappears;
child spend is persisted once in the root ledger before settlement.

Executable extensions require full-access mode and use your OS authority.
Install/enable/trust are separate; [publication-gated package setup](installation.md#optional-packages).
The bundled implementation is API 0.2, not a current-API authoring example.

## Extend or embed

- Add repository instructions, prompts, and skills through [instructions](instructions.md) and [resource discovery](resources.md).
- Add tools in any language through [extensions](extensions.md) and [Extension API 0.3](extensions/API-0.3-REFERENCE.md). A qualified end-to-end 0.3 example remains missing; do not relabel bundled 0.2 implementations.
- Use [browser](../extensions/octet-browse/README.md), [web search](../extensions/octet-web-search/README.md), or [MCP](../extensions/octet-mcp/README.md) through their package guides.
- Inventory/import/restore Pi setup with [Pi migration](pi-migration.md).
- Embed through the independent [native host protocol 1](sdk.md), or use the optional [graphical Serve interface](experimental/octet-serve/README.md).
