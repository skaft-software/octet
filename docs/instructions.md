# Instructions, prompts, and skills

[Documentation](README.md) · [Resource discovery](resources.md) · [Configuration](configuration.md)

Pick a reviewed prompt source and inspect its expansion before sending:

```sh
octet --prompt-template ./examples/prompts \
  --prompt local-review --debug-prompt "Review the parser without editing."
```

This uses the repository's [prompt examples](../examples/prompts/README.md).
Check the included content before it reaches a provider. The debug output can
contain it too.

## Repository instructions

Global and trusted workspace `AGENTS.md` files are added to octet's base prompt,
root to leaf. Project instructions need `--workspace-trusted`. Each block is
labeled with its path. Ordinary repository files, tool results and external
content stay data, never instructions. Relative tool paths and bash's default
directory resolve from the workspace root, which may not be where you launched
octet.

`--no-context-files` leaves context files out, and `/reload` recomposes
instructions and resources at a safe point. To replace the base prompt and the
AGENTS and context composition, set `system_prompt`, `OCTET_SYSTEM_PROMPT` or
`--system-prompt TEXT`. That value replaces the AGENTS and context composition,
but bootstrap still appends the discovered skill metadata and the file-location
catalog. A bare `--system-prompt` means explicit empty text, not the normal
default. See [precedence](configuration.md#precedence).

## Prompt templates

Markdown and TOML templates take arguments and bounded file inclusions. Global
templates live in `~/.octet/prompts/*.{md,toml}`, trusted projects in
`.octet/prompts/*.{md,toml}`. Repeatable `--prompt-template FILE-OR-DIR` sources
win. [Discovery limits](resources.md#reads-and-diagnostics).

| To do this | Use |
| --- | --- |
| List discovered templates | `/prompt` |
| Expand one interactively | `/prompt <name> [arguments]` or `/<name> [arguments]` |
| Select one at startup or in print mode | `octet --prompt <name> "arguments"` |
| Show the exact expansion and template hash before sending | `--debug-prompt` |

Markdown frontmatter is Pi-compatible (`$1`, `$@`, defaults, slices). TOML
templates and the variables `{{prompt}}`, `{{workspace}}`, `{{selection}}`,
`{{file:path}}` and `{{skill:name}}` also work. Interactive `{{selection}}` uses
the transcript selection, never the system clipboard, and is empty at startup
and in print mode. `argument-hint` shows in autocomplete, not in the prompt.
Reads and expansion are bounded, and path traversal is rejected. The template
name and SHA-256 are saved in the session as provenance the model never sees.
[Template contract](design/octet-coding-agent.md#prompt-templates).

## Skills

A skill is a package you load on purpose. None is active by default. `/skills`
lists them. In the TUI, `/skills search`, `/skills show`, `/skills load`,
`/skills off` and `/skills reload` are the human commands.
[Examples](../examples/skills/README.md).

The model has no skill-specific search, load or resource tool. When the composed
prompt lists a skill, it gives the `SKILL.md` location and tells the model to
use the ordinary `read` tool. Discovery supplies metadata and a location, not
the skill body. Resolve references relative to the skill directory (the parent
of `SKILL.md`). Supporting text is limited to `references/` or `templates/`, the
normal sandbox and path policy still applies, and `read` must be enabled.

Skill activation is not a durable state in the terminal frontends:

- `/skills load NAME` resolves the skill and prefills `/skill:NAME`.
  Submitting that draft expands the `SKILL.md` body into an ordinary user message
  (plus optional arguments); it does not append a durable activation event.
  `/skills off` can record deactivation only for an activation already present on
  the branch.
- Plain, print, and RPC runs expand explicit `/skill:NAME` text as ordinary
  prompt content; they do not gain an activation path.

An inlined body is therefore subject to ordinary history and compaction: it may
be summarized away, and resume does not reconstruct a separate active-skill state
from it. This is not a promise of durable skill activation.

Skill reads are bounded. Discovery caps YAML frontmatter at 32 KiB and
`SKILL.md` at 256 KiB. A supporting text read is capped at 512 KiB and must stay
under `references/` or `templates/`. [Skill
contract](design/octet-coding-agent.md#skills). Where skills are found, and in
what order: [Skill roots](resources.md#skill-roots). [Resource
discovery](resources.md) also covers managed-bundle admission, `--skill-dir`
order and diagnostics.

## Extension authoring

Executable tools in any language belong at the subprocess boundary. Start with
[extensions](extensions.md), and teach new code from [Extension API
0.4](extensions/API-0.4-REFERENCE.md). The Python SDK implements the current
feature-negotiated process wire. The generated `api_v03` bindings and the
retained canonical 0.3 example serve a separate wire. Keep exact versions and
negotiation, and don't retag an old example. Extensions add tools and bounded,
host-shaped integrations, not arbitrary replacement of host policy or UI. Native
embedding uses the separate [host protocol 1](sdk.md).

For the retained feature-negotiated machinery (API 0.2 and 0.4), see the [legacy
protocol reference](extensions/PROTOCOL-REFERENCE.md). The coding product
doesn't configure approval issuance or secret brokerage, so policy requests stay
default-deny and `secrets` isn't offered. That doesn't promise that every host
or frontend offers every protocol service. Discovery, trust, startup and reload
rules are in [resources](resources.md) and [extensions](extensions.md).

## Self-documentation

octet can answer questions about itself. Its default system prompt gives the
model the absolute paths of `README.md`, `docs/`, `examples/` and `sdk/`, which
sit beside the binary's assets, and asks it to read them first. The
[documentation index](README.md) is canonical. The public manual is on [the
octet website](https://skaft.org/octet/): compare its displayed release version
with your binary.

<details>
<summary>Where the docs live for each install type</summary>

Shell-installer layouts use `share/octet/`, and `OCTET_PACKAGE_DIR` or
`OCTET_DATA_DIR` overrides that root. Cargo-channel layouts embed the text
assets and write a versioned copy under the Cargo root's `share/octet/`,
refreshed after a Cargo update. The finite [public documentation
inventory](package-assets.txt) also includes linked security, licensing,
extension-reference and source-text files. Native, npm and container layouts
keep the listed non-text assets, and the embedded fallback stays text-only. npm
normalizes Git ignore metadata filenames on installation, and [the npm packaging
contract](release/npm-trusted-publishing.md#local-release-gate) records that
exception. Reference files don't install or enable extensions. These are source
layout contracts, **not evidence of published channels**.

In a source checkout, the prompt points to the checkout's `README.md`, `docs/`,
`examples/`, `sdk/`, `crates/` and the `octet-coding-agent` crate. Website
deployment and package publication are verified separately, and neither is
needed to read the bundled docs offline. See
[availability](installation.md#binary-availability).

</details>
