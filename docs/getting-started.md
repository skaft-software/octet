# Getting started

[Documentation](README.md) · [Installation](installation.md)

From a repository, start octet with approval gates:

```sh
octet --safe-mode --model claude-sonnet-4-6
```

```text
Explain how this project handles authentication. Show the relevant files.
Do not change anything.
```

## Before you start

1. [Install octet](installation.md). If building from source, use the full path to your built binary in place of `octet` in examples.
2. [Configure a cloud or local provider](providers.md). For Anthropic, set `ANTHROPIC_API_KEY`; for a first local model, interactive setup offers LM Studio or an explicitly selected OpenAI-compatible endpoint.
3. Work in an appropriate OS isolation boundary. **Full access is the default** when `--safe-mode` is absent. Safe mode asks before file changes and every shell call, but is not a sandbox and does not start executable extensions.

Use `--workspace-trusted` only after reviewing the project's configuration and
instructions. Without it, project configuration, `AGENTS.md`, and project
resources are not loaded. [Tools and permissions](tools.md).

## Next steps

- [Basic usage](workflows.md): ask for a change, review the result, and resume.
- [Terminal](terminal.md): plain/print modes, input, scrolling, and disclosure.
- [Sessions](sessions.md): durable history, branches, export, and recovery.
- [Configuration](configuration.md), [CLI](cli.md), and [slash commands](commands.md).
- [Images and audio](media.md), [context](context.md), and [instructions](instructions.md).

## Source identity

Source names are `octet`, `octet-host`, `octet-*`, `OCTET_*`, and `.octet`.
There are no Ygg aliases, old-root readers, or automatic Hamr/Ygg imports.
The read-only third-party Codex credential source and explicit
[Pi import/restore](pi-migration.md) are separate interoperability contracts;
original source stores are not modified. See the [0.7.1 notes](releases/v0.7.1.md)
for outstanding qualification gates.
