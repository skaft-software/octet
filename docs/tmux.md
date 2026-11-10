# tmux setup

[Documentation](README.md) · [Terminal](terminal.md) · [CLI](cli.md)

octet works inside tmux, but tmux strips modifier information from certain keys
by default. Without configuration, `Shift+Enter` and `Ctrl+Enter` usually arrive
as plain `Enter`, so the "newline without submitting" binding is unavailable.

## Recommended configuration

Add to `~/.tmux.conf`:

```tmux
set -g extended-keys on
set -g extended-keys-format csi-u
```

Then restart tmux fully (`tmux kill-server && tmux`).

On Unix, octet requests Kitty keyboard flags 1|4 (`\x1b[>5u`) on the active
screen, without changing the ordinary text path. It does not query support or
request `modifyOtherKeys` mode. If a terminal or multiplexer emits xterm
`modifyOtherKeys` for modified Enter, octet also decodes that form
(`Shift+Enter` → `\x1b[27;2;13~`).

- `set -g extended-keys on` makes tmux forward modified keys at all.
- `extended-keys-format csi-u` (tmux **3.5+**) forwards them as CSI-u
  (`Shift+Enter` → `\x1b[13;2u`), which is the most reliable form.
- With older tmux (3.2–3.4), omit `extended-keys-format`; tmux's default
  `modifyOtherKeys` form is still understood.

Check the version with `tmux -V`. A terminal emulator that supports extended
keys (Ghostty, Kitty, iTerm2, WezTerm, Windows Terminal, …) is also required.

| Key | Without extended keys | With `extended-keys on` + `csi-u` |
| --- | --- | --- |
| Enter | `\r` | `\r` |
| Shift+Enter | `\r` | `\x1b[13;2u` |
| Ctrl+Enter | `\r` | `\x1b[13;5u` |
| Alt/Option+Enter | `\x1b\r` | `\x1b[13;3u` |

This affects the default bindings (`Enter` submits, `Shift+Enter` inserts a
newline) and any modified-Enter binding. If both keys arrive as `\r`, octet
cannot distinguish them and Shift+Enter may submit; Ctrl+J inserts a newline
instead. See [terminal support](terminal.md#shiftenter-terminal-support) and
[Commands and keys](commands.md#keys).

## Detection and separate behaviors

octet detects tmux from the `TMUX` environment variable
(`TerminalCapabilities::from_process` in `crates/sexy-tui-rs/src/capabilities.rs`). Inside tmux/screen it keeps
hyperlinks disabled unless overridden, and Kitty graphics are not assumed.
Mouse scrolling and transcript selection keep working through tmux's own mouse
reporting; see [terminal scrolling](terminal.md#scrolling-and-rendering).
