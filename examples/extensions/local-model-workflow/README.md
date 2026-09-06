# local-model-workflow executable extension

**Legacy Python runtime example**, not an API `0.3` quickstart. Its
`before_prompt` and `context/collect` interfaces are legacy contribution points,
not current API `0.3` authoring surfaces. See [current authoring](../../../docs/extensions.md)
and the [legacy runtime](../../../sdk/python/legacy-runtime.md); do not retag its
manifest.

The dependency-free SDK keeps local-model prompt shaping explicit and inspectable:

- `before_prompt` returns compact, labeled system-suffix context.
- `context/collect` returns the same deterministic text for prompt composition
  and context inspection.
- A semantic status item uses current model and active-skill metadata.
- One process-originated notification reports when shaping first becomes active.

For an existing legacy setup, install the source SDK from a checkout:

```console
python3 -m pip install ./sdk/python
```

Copy to `.octet/extensions/local-model-workflow/`, explicitly enable and trust
it, then restart or use `/extensions reload`. `--safe-mode` discovers the
manifest but never starts it. Full-access requires a trusted, separately isolated
environment; declarations are not an OS sandbox.

Typed hooks, context, status, and events remain protocol contributions, but the
coding TUI does not render generic persistent extension status. The extension
reads no files, launches no child subprocesses, uses no network, and emits no
terminal escapes.

Context is intentionally short for small windows, deterministic for the same
host state, and exposes its label and placement so users can inspect exactly
what reaches the model.
