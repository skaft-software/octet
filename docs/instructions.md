# Instructions, prompts, and skills

[Documentation](README.md) · [Resource discovery](resources.md) · [Configuration](configuration.md)

Select a reviewed prompt source explicitly, then inspect its expansion:

```sh
octet --prompt-template ./examples/prompts \
  --prompt local-review --debug-prompt "Review the parser without editing."
```

This uses the repository's [prompt examples](../examples/prompts/README.md).
Inspect included content before allowing it to reach a provider; debug output
can contain that content too.

## Repository instructions

Global and trusted workspace `AGENTS.md` files compose with octet's base prompt
in root-to-leaf precedence. Project instructions require `--workspace-trusted`.
The host labels these blocks with their paths; ordinary repository files, tool
results, and external content remain data, not instructions. Relative tool
paths and bash's default cwd resolve from the workspace root, not necessarily
the invocation directory.

Use `--no-context-files` to omit context files. `/reload` recomposes current
instructions and resources at a safe boundary. To replace **all** composed system
instructions, set `system_prompt`, `OCTET_SYSTEM_PROMPT`, or
`--system-prompt TEXT`. AGENTS/context/skills are ignored while this override is
set. Bare `--system-prompt` means explicit empty text, not the normal default.
See the [configuration precedence](configuration.md#precedence).

## Prompt templates

Markdown and TOML templates accept arguments and bounded file inclusions.
Global sources are `~/.octet/prompts/*.{md,toml}`; trusted projects use
`.octet/prompts/*.{md,toml}`. Repeatable `--prompt-template FILE-OR-DIR` sources
have highest precedence. [Discovery bounds](resources.md#reads-and-diagnostics).

| Invocation | Use |
| --- | --- |
| `/prompt` | Inspect discovered templates. |
| `/prompt <name> [arguments]` or `/<name> [arguments]` | Expand interactively. |
| `octet --prompt <name> "arguments"` | Startup/print selection. |
| `--debug-prompt` | Show exact expansion and template hash before provider submission. |

Pi-compatible Markdown frontmatter supports `$1`, `$@`, defaults, and slices.
TOML and deterministic `{{prompt}}`, `{{workspace}}`, `{{selection}}`,
`{{file:path}}`, and `{{skill:name}}` variables are also supported.
Interactive `{{selection}}` uses semantic transcript selection without reading
or writing the system clipboard; startup/print expands it to empty text.
`argument-hint` appears in autocomplete, never in the composed prompt.

Template/included-file reads and final expansion are bounded; traversal is
rejected. Selection name and SHA-256 persist as non-model-visible session
provenance. Detailed syntax remains in the
[product template contract](design/octet-coding-agent.md#prompt-templates).

## Skills

Skills are explicit, inspectable packages, not automatically active instructions.
Discover them with `/skills`; load a reviewed skill with `/skills load NAME`.
`/skills ...` supports listing/search, inspection, activation, unloading, and
reload, including `/skills reload`. [Examples](../examples/skills/README.md).

The host discovers metadata, activates only selected skills, injects active
instructions once, and reads references lazily under bounds. Activation and
resource snapshots are durable session events and survive compaction. The
model-facing `search_skills`, `load_skill`, and `read_skill_resource` tools use
that same boundary: loading checks trust and required tools; resource reads
require a matching active hash and text under `references/` or `templates/`.
Changed instructions are rejected rather than silently mixed into an activation.
[Skill contract](design/octet-coding-agent.md#skills).

For the complete low-to-high skill roots and project-trust boundary, see
[skill discovery](resources.md#skill-roots). [Resource discovery](resources.md)
also defines managed-bundle admission, `--skill-dir` order, and diagnostics.

## Extension authoring

Executable tools in any language belong at the subprocess boundary. Start with
[extensions](extensions.md) and teach new code from
[Extension API 0.3](extensions/API-0.3-REFERENCE.md). Existing API 0.2 SDK/runtime
examples and the four bundled manifests are **legacy implementation references**;
do not relabel their versions or wire IDs. Generated Python 0.3 types are not a
complete 0.3 `Extension` runtime, so a qualified end-to-end current-API example
remains missing. Native embedding uses the independent [host protocol 1](sdk.md).

For existing API 0.2 machinery—live tool registration/removal, request-frozen
catalogs, owner-bound child sessions, session/process ownership, artifacts,
policy intents/one-use approvals, manifest-allowlisted secrets, and bounded
post-handshake restart—keep the [legacy protocol reference](extensions/PROTOCOL-REFERENCE.md).
The unreleased source coding product configures generic one-use exact-call
approvals only for isolated API `0.2` `octet-mcp`, not blanket server permission
or #383's typed automation policy. Other policy requests remain default-deny;
no host secret broker is configured and `secrets` is not offered. This is not a
promise that API 0.3 provides every legacy service. Discovery/trust/startup
and reload rules remain in [resources](resources.md) and [extensions](extensions.md).

## Self-documentation

The packaging contract places `README.md`, `docs/`, `examples/`, and `sdk/`
beside the binary's assets. The default system prompt points to their absolute
paths and asks the model to read them for octet questions/changes. Shell-installer
layouts use matching `share/octet/`; `OCTET_PACKAGE_DIR` or `OCTET_DATA_DIR`
overrides that root. Cargo-channel layouts embed the text assets and materialize
a versioned copy under the Cargo root's `share/octet/`, refreshed after a Cargo
update. These are source layout contracts, **not evidence of published channels**.

From a source checkout, the prompt instead points to its `README.md`, `docs/`,
`examples/`, `sdk/`, `crates/`, and `octet-coding-agent` crate. The
[documentation index](README.md) is canonical. `https://skaft.org/octet` is a
proposed source website identity, not publication verification or a requirement
for reading these contracts. [Availability](installation.md#binary-availability).
