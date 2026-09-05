# caffeinate executable extension

**Legacy API `0.2` example**, version `0.2.0`; not an API `0.3` quickstart. See
[current authoring](../../../docs/extensions.md) and the
[legacy Python runtime](../../../sdk/python/legacy-runtime.md). Keep its manifest
version unchanged when studying it.

This Python extension keeps a Mac awake while octet owns one or more active
root turns. Sleep inhibition lives in the extension, not the kernel. It observes
`turn/started`, `turn/settled`, and `session/settled`, reference-counts overlapping
turns, and runs one `/usr/bin/caffeinate -i -t 1800` subprocess until the last
observed turn settles.

`-i` prevents idle system sleep without forcing the display on or overriding
explicit sleep choices. `-t 1800` bounds the assertion to 30 minutes if cleanup
cannot be delivered. The example does not use `-w` and does not bind the helper
to the extension PID. `/caffeinate` reports whether inhibition is active. It also
supplies an `awake` semantic status contribution; the coding TUI does not render
generic persistent extension status. Unsupported systems remain usable and
receive a diagnostic when a turn starts.

For an existing legacy setup, install the source SDK from a checkout:

```console
python3 -m pip install ./sdk/python
```

Copy the directory to `.octet/extensions/caffeinate/`, then enable and trust it:

```console
octet --workspace-trusted \
    --enable-extension caffeinate \
    --trust-extension caffeinate
```

Startup requires default full-access policy; `--safe-mode` keeps it stopped.
Full-access uses the octet process's ambient OS authority: run only in a trusted,
appropriately isolated environment. The extension requires macOS and
`/usr/bin/caffeinate`, reads no files, and uses no network. Its `process = true`
declaration is consent metadata for the helper, not an OS sandbox.

API `0.2` settles completed, failed, interrupted, and cancelled root turns, so
each terminal releases its reference. Session settlement clears remaining
references for that session. Extension shutdown and top-level protocol cleanup
explicitly terminate the helper; the 30-minute timeout is a final fail-safe.

The example's dependency-free test command, from the repository root:

```console
python3 examples/extensions/caffeinate/test_extension.py
```
