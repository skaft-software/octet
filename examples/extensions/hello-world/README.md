# hello-world executable extension

**Legacy API `0.1` example**, not an API `0.3` quickstart. See
[current authoring](../../../docs/extensions.md) and the
[legacy Python runtime](../../../sdk/python/legacy-runtime.md). Keep the manifest's
wire version unchanged; generated API `0.3` types are not a complete runtime.

This dependency-free Python SDK example demonstrates legacy initialization,
a model tool, slash command, request-path hooks, prompt context, semantic status,
tool renderer, and notification. Legacy request-path hooks are not API `0.3`
paired session-cleanup hooks; generic status and renderer contributions are not
persistent coding-TUI chrome.

For an existing legacy setup, install the source SDK from a checkout:

```console
python3 -m pip install ./sdk/python
```

Copy to `.octet/extensions/hello-world/` and explicitly enable and trust
`hello-world` before restarting or reloading extensions. `--safe-mode` discovers
but never starts it. Full-access startup uses ambient OS authority; use a trusted,
separately isolated environment.

The host resolves bare `extension.py` beside `extension.toml` and launches it
directly. Python 3 must be available through the shebang environment. Stdout stays
protocol-only; the SDK sends structured diagnostics to stderr.
