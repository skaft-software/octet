# git-tools executable extension

**Legacy API `0.1` example**, not an API `0.3` quickstart. See
[current authoring](../../../docs/extensions.md) and the
[legacy Python runtime](../../../sdk/python/legacy-runtime.md). Do not retag the
manifest to claim a current-API implementation.

This dependency-free Python SDK example contributes:

- `git_status`: bounded arguments, a five-second timeout, bounded output, and
  metadata. The API `0.1` native adapter discards metadata; it has no frontend,
  renderer, or persistence guarantee.
- `/checkpoint [label]`: a deliberately read-only checkpoint preview.
- A semantic `git_status` renderer returning theme roles, not ANSI escapes.
  Renderer output remains internal provenance, not coding-TUI rendered evidence.

For an existing legacy setup, install the source SDK from a checkout:

```console
python3 -m pip install ./sdk/python
```

Copy to `.octet/extensions/git-tools/`, then explicitly enable and trust
`git-tools`. Default full-access launches it; `--safe-mode` discovers it but
will not start it. Use a trusted, separately isolated environment.

Git must be on `PATH`. The extension runs only read commands, sets
`GIT_OPTIONAL_LOCKS=0`, never invokes a shell, and does not create commits.
`process = true` is consent metadata for `git status`, not an OS sandbox.
