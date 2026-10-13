# Custom resource discovery

Prompts, skills and executable extensions share one filesystem resolver. Each
type's parser owns its format. The resolver owns the safety and precedence
rules. The built-in `auto`, `light` and `dark` appearances work through `/theme`
without theme files. Named theme files follow the separate, bounded [theme
loader](themes.md).

## Locations and precedence

| Kind | Global | Trusted project | Explicit option |
| --- | --- | --- | --- |
| Prompt | `~/.octet/prompts/*.{md,toml}` | `.octet/prompts/*.{md,toml}` | `--prompt-template <file-or-dir>` |
| Skill | [Ordered user roots](#skill-roots) | [Ordered project roots](#skill-roots) | `--skill-dir` |
| Extension | `~/.octet/extensions/*/extension.toml` | `.octet/extensions/*/extension.toml` | `--extension-dir` |

Roots are visited global, then project, then explicit in option order. An
explicit prompt source can be one `.md` or `.toml` file, or a directory. A later
definition with the same name wins, and the shadowed path stays in the
diagnostics. Scans and ordering are deterministic.

A valid package-manager `install.json` admits an installed bundle's nested
`skills/` root. Copying an extension directory in by hand doesn't. Bundle skills
rank below `~/.octet/skills`, stay inactive until loaded, and disappear from
discovery once the package is removed.

Workspace resources are ignored without `--workspace-trusted`. Explicit paths
are your choice for that run.

Extensions add a second boundary. Discovery and workspace trust never launch
code. Installed executable extensions are **disabled by default**. Full access
(`unsafe_host`, the default) implicitly trusts the selected, validated source,
so an extension you've enabled needs no extra trust flag. Trust and enablement
are separate, and implicit trust never writes a grant to config. Process startup
still respects `--no-process` and `--no-shell`.

`--safe-mode` removes implicit host authority, so enabled extensions start only
with an explicit grant for the selected source. A grant lets code run as a host
process with your OS permissions, outside the tool-effect broker. Safe mode
isn't an extension sandbox. A project config can't create a persistent host
authority grant. A bare name in persistent trust applies only to the global
extension directory. Project and explicit sources need an exact absolute
`name@.../extension.toml` grant to persist host authority.
`--trust-extension NAME` grants it for that run only, and never enables
anything. `--extension-dir` is itself a one-run grant for that source. Grants
don't transfer between sources, and they permit startup under safe mode. The
extension directory name must match the manifest name, and source,
compatibility, bundle and artifact validation stay mandatory.

If octet can't resolve an absolute home directory, it disables global config and
resources with a diagnostic. It never falls back to the invocation directory and
treats project files as user-owned.

### Skill roots

Skill discovery goes from **lowest to highest precedence**:

1. **User:** `~/.agents/skills`, then `~/.pi/agent/skills`, then managed
   extension skills, then `~/.octet/skills`.
2. **Trusted project:** `.agents/skills` roots from the workspace through the
   invocation directory, then the invocation directory's `.pi/skills`, then the
   workspace's `.octet/skills`. These need `--workspace-trusted`.
3. **Explicit:** `--skill-dir` sources, in command-line order.

A project root that's also a user skill root is scanned only in the user tier.
For example, starting in your home directory keeps `~/.agents/skills` and
`~/.octet/skills` user-installed, with no untrusted-project warning for those
roots. That doesn't trust the workspace: distinct project roots, including
nested `.agents/skills` and the invocation's `.pi/skills`, stay gated. Root
symlinks are still rejected.

A later definition replaces an earlier one with the same skill name, and
collisions go in the diagnostics. Discovery never activates a skill. The native
entrypoints are `~/.octet/skills/*/SKILL.md`, `.octet/skills/*/SKILL.md` and
managed `~/.octet/extensions/*/skills/*/SKILL.md`. These locations don't promise
full Agent Skills or Pi parser compatibility, or extra symlink support. Those
shapes need their own source contract.

### Skill catalog budgets

The skill catalog the model sees is capped: 1024 bytes per description, 256
descriptors / 256 KiB of payload, and 64 KiB of rendered text. Leaving a skill
out of the catalog doesn't deactivate it.

<details>
<summary>The exact caps and what omission means</summary>

Discovery accepts at most 32 KiB of YAML frontmatter per skill and scans at most
4096 entries per root. Those are per-input limits. Across **all roots**, the
retained catalog also has these caps:

- **1024 UTF-8 bytes per description**, including an ellipsis when shortened.
  Only the metadata excerpt is shortened. The source file and instruction body
  are unchanged, and the full description allocation isn't kept.
- **256 descriptors / 256 KiB of descriptor payload**, whichever comes first.
  Payload counts text fields, encoded paths and JSON-serialized arbitrary
  metadata, not allocator overhead. Admission follows deterministic root and
  candidate order. A later definition still replaces an earlier one with the
  same ID, and if a larger replacement doesn't fit, its predecessor is removed
  rather than advertised as the winner.
- **64 KiB of rendered model-catalog text**, including XML escaping, framing,
  paths and any cap notice. Rendering uses ID and path order and stops before
  the first entry that doesn't fit. Names, location paths, XML entities and
  closing tags are never cut. `disable-model-invocation` skills stay excluded.

Description caps and omitted counts appear in discovery diagnostics. Winning
source locations stay indexed, so a known `/skill:NAME` or `/skills load NAME`
can still load an omitted skill, with the usual trust, required-tool, symlink
and 256 KiB file limits. An omitted header is parsed on demand, and a changed
skill ID needs rediscovery. Listing and search use the bounded descriptors, not
the full source index. These limits bound retained descriptors and model
context, not total discovery work or RSS: the lightweight source index and
diagnostics still grow with discovered inputs.

</details>

## Reads and diagnostics

octet-native resource roots, selected files and directory entrypoints must be
regular, non-symlink files. Parser reads use descriptor-bound no-follow opens
and fixed byte limits:

| Kind | Maximum parser input |
| --- | ---: |
| Prompt | 512 KiB |
| Skill entrypoint | 256 KiB |
| Extension manifest selected by the product resource resolver | 256 KiB |

Invalid UTF-8, bad names, inaccessible roots, rejected links, oversized files,
parser failures and precedence decisions all show up as diagnostics. One broken
customization doesn't stop octet starting.

Automatic reload doesn't repeat resource, bootstrap or keybinding problems for
each checked component. A successful check clears that component's remembered
problem, so a later recurrence shows up. Skipped checks don't clear it. Explicit
commands still report their diagnostics, and real work losses are never hidden
as duplicate configuration warnings.

<details>
<summary>Other limits</summary>

After discovery, prompt expansion, skill resource reads, extension protocol
messages and session files each have their own narrower limits.
`ExtensionManifest::load` has a separate 64 KiB default. Product discovery reads
the selected manifest within the 256 KiB bound, then calls
`ExtensionManifest::parse`.

</details>

## Reload

Each discovery pass makes an immutable snapshot. octet builds a complete
replacement from it and swaps only after validation, so a running prompt never
sees half a reload.

- `/skills reload` refreshes prompts and skills.
- `/extensions reload` handshakes replacement processes when enablement and host
  authority allow startup. Safe mode starts granted sources, not ungranted ones.
- `/reload` runs full discovery and rebuilds the active customization.
