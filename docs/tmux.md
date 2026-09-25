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

octet asks capable terminals for unambiguous modified controls at startup with
`\x1b[>7u` (Kitty keyboard flags 1|2|4) followed by a device-attributes query;
when the terminal is not in the Kitty family it falls back to
`modifyOtherKeys` (`\x1b[>4;2m`). Both use the terminal's normal input path for
ordinary text (`crates/sexy-tui-rs/src/terminal.rs:53`).

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
newline) and any modified-Enter binding — see
[Commands and keys](commands.md#keys).

## Detection and separate behaviors

octet detects tmux from the `TMUX` environment variable
(`crates/sexy-tui-rs/src/capabilities.rs:392`). Inside tmux/screen it keeps
hyperlinks disabled unless overridden, and Kitty graphics are not assumed.
Mouse scrolling and transcript selection keep working through tmux's own mouse
reporting; see [terminal scrolling](terminal.md#scrolling-and-rendering).
