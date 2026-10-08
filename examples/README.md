# octet customization examples

Start with a prompt or skill and copy it into the matching project `.octet/`
directory. These resources use normal filesystem discovery and typed
contribution boundaries:

- [Prompts](prompts/README.md): Pi-compatible Markdown and compact octet TOML.
- [Skills](skills/README.md): explicit, inspectable skills with bounded text
  resources.
- [Pi migration skill](skills/pi-migration/SKILL.md): low-token cleanup around
  the zero-token Pi inventory.

## Current extension authoring

Start from the [Python API 0.4 tool
recipe](../sdk/python/README.md#minimal-api-04-tool) for a bounded local
process. The [canonical API 0.3 example](extensions/api-v03-minimal/README.md)
and its generated conformance suite stay live, with their exact versions and
host pins. This checkout is a local release candidate, not a new SDK or bundle
publication.

## Legacy extension examples

These are **legacy implementation references**, not extension authoring
quickstarts. New extensions use [API `0.4`](../docs/extensions.md). The Python
`Extension` runtime handles the feature-negotiated API `0.4` wire and the
retained API `0.1`/`0.2`, and the generated API `0.3` types serve the separate
canonical wire. Don't retag these examples.

- [hello-world](extensions/hello-world/README.md): API `0.1` initialization,
  model tool, command, hooks, context, status, renderer and notification.
- [caffeinate](extensions/caffeinate/README.md): API `0.2` macOS sleep
  inhibition during owning and root turns, with bounded cleanup.
- [git-tools](extensions/git-tools/README.md): API `0.1` read-only checkpoint,
  bounded git-status tool, semantic status contribution and renderer. Generic
  extension status doesn't become persistent coding-TUI chrome.
- [local-model-workflow](extensions/local-model-workflow/README.md): legacy
  deterministic prompt and context shaping, and semantic status, for small
  contexts.
- [lsp-client](extensions/lsp-client/README.md): legacy read-only LSP
  `definition`, `references`, `hover` and diagnostics through one
  `code_intelligence` tool, with bounded, typed unavailable states.

For an existing legacy setup, install the dependency-free source SDK from a
checkout before copying an extension:

```console
python3 -m pip install ./sdk/python
```

Executable extensions need separate enablement and exact trust. They run under
the default full-access policy and stay stopped under `--safe-mode`. Capability
declarations aren't an OS sandbox, so use a trusted, separately isolated
environment. See [discovery and
trust](../docs/extensions.md#layout-and-discovery) and the [legacy Python
runtime](../sdk/python/legacy-runtime.md).
