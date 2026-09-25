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
instructions and resources at a safe boundary. To replace the base and
AGENTS/context composition, set `system_prompt`, `OCTET_SYSTEM_PROMPT`, or
`--system-prompt TEXT`. AGENTS/context composition is replaced by that value;
bootstrap still appends the discovered skill metadata and file-location catalog.
Bare `--system-prompt` means explicit empty text, not the normal default.
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
Discover them with `/skills`; in the TUI, `/skills search`, `/skills show`,
`/skills load`, `/skills off`, and `/skills reload` are human commands.
[Examples](../examples/skills/README.md).

The model has no skill-specific search, load, or resource tool. When the composed
prompt lists a skill, it supplies the `SKILL.md` location and directs the model to
use the ordinary `read` tool; discovery supplies metadata and a location rather
than injecting the skill body. Resolve references relative to the skill directory
(parent of `SKILL.md`). For package resource semantics, supporting text is limited
to `references/` or `templates/`; normal sandbox/path policy still applies, and
`read` must be enabled.

TUI and Serve do not have the same activation contract:

- In the TUI, `/skills load NAME` resolves the skill and prefills `/skill:NAME`.
  Submitting that draft expands the `SKILL.md` body into an ordinary user message
  (plus optional arguments); it does not append a durable activation event.
  `/skills off` can record deactivation only for an activation already present on
  the branch.
- Serve's slash-command worker appends a durable `SkillActivated` event on load
  and `SkillDeactivated` on off. The activation event is Serve-only; TUI `off`
  can only deactivate pre-existing state. Plain, print, and RPC do not gain this
  activation path; their prompt preparation only expands explicit `/skill:NAME`
  text as ordinary prompt content.

An inlined TUI body is therefore subject to ordinary history and compaction: it may
be summarized away, and resume does not reconstruct a separate active-skill state
from it. Serve activation state can be reconstructed from its session events and
compaction snapshots; this is not a promise of durable TUI activation.

Skill package resource reads are bounded: discovery caps YAML frontmatter at 32
KiB and `SKILL.md` at 256 KiB; a supporting text read is capped at 512 KiB and
must stay under `references/` or `templates/`.
[Skill contract](design/octet-coding-agent.md#skills).
For the complete low-to-high skill roots and project-trust boundary, see
[skill discovery](resources.md#skill-roots). [Resource discovery](resources.md)
also defines managed-bundle admission, `--skill-dir` order, and diagnostics.

## Extension authoring

Executable tools in any language belong at the subprocess boundary. Start with
[extensions](extensions.md) and teach new code from
[Extension API 0.4](extensions/API-0.4-REFERENCE.md). The Python SDK implements
the current feature-negotiated process wire; generated `api_v03` bindings and
the retained canonical 0.3 example serve a distinct wire. Keep exact versions
and negotiation—do not retag an old example. Extensions add tools and bounded
host-shaped integrations, not arbitrary replacement of host policy or UI.
Native embedding uses the independent [host protocol 1](sdk.md).

For retained feature-negotiated machinery (API 0.2/0.4)—live tool registration/removal, request-frozen
catalogs, owner-bound child sessions, session/process ownership, artifacts,
policy intents/one-use approvals, manifest-allowlisted secrets, and bounded
post-handshake restart—keep the [legacy protocol reference](extensions/PROTOCOL-REFERENCE.md).
The coding product does not configure approval issuance or secret brokerage:
policy requests remain default-deny and `secrets` is not offered. This is not a
promise that every host or frontend offers every protocol service. Discovery/trust/startup
and reload rules remain in [resources](resources.md) and [extensions](extensions.md).

## Self-documentation

The packaging contract places `README.md`, `docs/`, `examples/`, and `sdk/`
beside the binary's assets. The default system prompt points to their absolute
paths and asks the model to read them for octet questions/changes. Shell-installer
layouts use matching `share/octet/`; `OCTET_PACKAGE_DIR` or `OCTET_DATA_DIR`
overrides that root. Cargo-channel layouts embed the text assets and materialize
a versioned copy under the Cargo root's `share/octet/`, refreshed after a Cargo
update. The finite [public documentation inventory](package-assets.txt) also
includes linked security, licensing, extension-reference and source-text files.
Native, npm and container layouts retain the listed non-text assets; the embedded
fallback remains text-only. npm normalizes Git ignore metadata filenames on
installation; [the npm packaging contract](release/npm-trusted-publishing.md#local-release-gate)
records that exception. Reference files do not install or enable extensions.
These are source layout contracts, **not evidence of published channels**.

From a source checkout, the prompt instead points to its `README.md`, `docs/`,
`examples/`, `sdk/`, `crates/`, and `octet-coding-agent` crate. The
[documentation index](README.md) is canonical. The public manual is hosted at
[the octet website](https://skaft.org/octet/); compare its displayed release
version with the binary. Website deployment and package publication are verified
independently, and neither is required to read the bundled contracts offline.
[Availability](installation.md#binary-availability).
