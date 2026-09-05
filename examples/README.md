# octet customization examples

These examples are small, copyable starting points for a local octet setup. They
use the same filesystem discovery and typed contribution boundaries as normal
user resources.

The Python extension examples use the dependency-free `octet-extension-sdk`.
Install it with `python3 -m pip install ./sdk/python` before copying an
extension into a project.

- [`prompts/`](prompts/) — Pi-compatible Markdown prompts and octet's compact
  TOML form.
- [`skills/`](skills/) — explicit, inspectable skills with bounded text
  resources.
- [`skills/pi-migration/`](skills/pi-migration/) — low-token cleanup around the
  zero-token Pi inventory.
- [`extensions/hello-world/`](extensions/hello-world/) — a minimal executable
  JSON-RPC extension.
- [`extensions/caffeinate/`](extensions/caffeinate/) — a macOS sleep inhibitor
  active while octet processes a prompt.
- [`extensions/git-tools/`](extensions/git-tools/) — a custom command, git
  status tool, status-line contribution, and tool renderer.
- [`extensions/local-model-workflow/`](extensions/local-model-workflow/) —
  deterministic prompt/context shaping for smaller local context windows.
- [`extensions/lsp-client/`](extensions/lsp-client/) — read-only LSP code
  intelligence (`definition`, `references`, `hover`, diagnostics) via one
  `code_intelligence` tool with bounded, typed unavailable states.

Copy an example into the matching `.octet/` directory in a project. Executable
extensions must also be explicitly enabled and independently trusted. They run
under octet's default full-access policy and remain stopped under `--safe-mode`;
see [`docs/extensions.md`](../docs/extensions.md).
