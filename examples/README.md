# octet customization examples

Start with a prompt or skill and copy it into the matching project `.octet/`
directory. These resources use normal filesystem discovery and typed contribution
boundaries:

- [Prompts](prompts/README.md): Pi-compatible Markdown and compact octet TOML.
- [Skills](skills/README.md): explicit, inspectable skills with bounded text resources.
- [Pi migration skill](skills/pi-migration/SKILL.md): low-token cleanup around the
  zero-token Pi inventory.

## Legacy extension examples

These are **legacy implementation references**, not current extension authoring
quickstarts. New extensions use [API `0.3`](../docs/extensions.md). The Python
`Extension` runtime handles API `0.1`/`0.2`; generated API `0.3` types do not
supply a complete `Extension` runtime. Do not retag these examples.

- [hello-world](extensions/hello-world/README.md): API `0.1` initialization,
  model tool, command, hooks, context, status, renderer, and notification.
- [caffeinate](extensions/caffeinate/README.md): API `0.2` macOS sleep inhibition
  during owning/root turns, with bounded cleanup.
- [git-tools](extensions/git-tools/README.md): API `0.1` read-only checkpoint,
  bounded git-status tool, semantic status contribution, and renderer. Generic
  extension status does not become persistent coding-TUI chrome.
- [local-model-workflow](extensions/local-model-workflow/README.md): legacy
  deterministic prompt/context shaping and semantic status for small contexts.
- [lsp-client](extensions/lsp-client/README.md): legacy read-only LSP
  `definition`, `references`, `hover`, and diagnostics through one
  `code_intelligence` tool with bounded, typed unavailable states.

For an existing legacy setup, install the dependency-free source SDK from a
checkout before copying an extension:

```console
python3 -m pip install ./sdk/python
```

Executable extensions require independent enablement and exact trust. They run
under the default full-access policy and stay stopped under `--safe-mode`.
Capability declarations are not an OS sandbox; use a trusted, separately
isolated environment. See [discovery and trust](../docs/extensions.md#layout-and-discovery)
and the [legacy Python runtime](../sdk/python/legacy-runtime.md).
