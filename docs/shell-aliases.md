# Shell aliases

[Documentation](README.md) · [Tools](tools.md) · [CLI](cli.md)

octet runs shell commands non-interactively. The `bash` tool starts the selected
Bash-compatible shell with `-c <command>` (`crates/octet-agent/src/tools/bash.rs:244`),
and the interactive `!<command>` local command runs through `sh -c`
(`crates/octet-coding-agent/src/modes/interactive.rs:5838`). Neither is a login
or interactive shell, so shell aliases defined in `~/.zshrc`, `~/.bashrc`, or a
zsh profile are **not** expanded.

## Workarounds

- Put reusable behavior in an executable **script on `PATH`** (or reference it by
  absolute path) and call that instead of an alias.
- Use the full command in the request, or ask octet to write a small wrapper.
- For interactive `!<command>`, remember that `!` output is captured into the
  transcript as a collapsible shell block; it is a local command, not a model
  tool call.

Shell execution is gated by the process/shell policy: `--no-process` and
`--no-shell` disable it, and safe mode requires approval for each call
([tools and policy](tools.md), [configuration](configuration.md)).
