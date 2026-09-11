# Custom resource discovery

Prompts, skills, and executable extensions share one filesystem
resolver. Resource-specific parsers own their schemas; the resolver owns the
cross-cutting local safety and precedence contract. Theme customization is
disabled in v0.7.0; the terminal uses its compiled default.

## Locations and precedence

| Kind | Global | Trusted project | Explicit option |
| --- | --- | --- | --- |
| Prompt | `~/.octet/prompts/*.{md,toml}` | `.octet/prompts/*.{md,toml}` | `--prompt-template <file-or-dir>` |
| Skill | [Ordered user roots](#skill-roots) | [Ordered project roots](#skill-roots) | `--skill-dir` |
| Extension | `~/.octet/extensions/*/extension.toml` | `.octet/extensions/*/extension.toml` | `--extension-dir` |

Roots are visited global, project, then explicit in option order. An explicit
Pi-compatible prompt source may be one `.md`/`.toml` file or a directory.
Later definitions with the same resource name win, and the shadowed path
remains in the diagnostic snapshot. Scans and result ordering are
deterministic. A valid package-manager `install.json` admits an installed
bundle's nested `skills/` root; merely copying an unmanaged extension directory
does not. Bundle skills have lower precedence than `~/.octet/skills`, remain
inactive until explicitly loaded, and disappear from the next discovery
snapshot after package removal.

Workspace resources are ignored until `--workspace-trusted` is present.
Explicit paths are an intentional user choice for that invocation. Executable
extensions add a second boundary: discovery and workspace trust still do not
launch code. Installed executable extensions remain **disabled by default**.
Full access (`unsafe_host`, the default) implicitly trusts the selected validated
source, so an explicitly enabled extension needs no extra trust flag. Trust and
enablement are separate; implicit trust never writes a grant to configuration.
Process startup still requires full access and the independent process gate.

`--safe-mode` removes implicit trust and blocks executable startup even when an
explicit grant exists. It does not sandbox extensions or allow approval to
bypass the `unsafe_host` floor. A project config cannot create a persistent
executable trust grant. Bare persistent trust names apply only to the global
extension directory; project and explicit sources require an exact absolute
`name@.../extension.toml` grant to persist trust. `--trust-extension name` grants
explicit trust only for that invocation, never enablement. These grants do not
transfer between sources or override safe-mode execution policy. The extension
directory name must match the manifest name, and source, compatibility, bundle,
and artifact validation remain mandatory.

If octet cannot resolve an absolute user home directory, global configuration and
global resources are disabled with a diagnostic. It never falls back to the
invocation directory and reclassifies project files as user-owned resources.

### Skill roots

Skill discovery uses this **low-to-high precedence** order:

1. **User:** `~/.agents/skills`, then `~/.pi/agent/skills`, then managed extension
   skills, then `~/.octet/skills`.
2. **Trusted project:** `.agents/skills` roots from the workspace through the
   invocation directory, then the invocation directory's `.pi/skills`, then the
   workspace's `.octet/skills`. These project roots require `--workspace-trusted`.
3. **Explicit:** `--skill-dir` sources in command-line option order.

A project root that is also a user skill root is scanned only in the user tier.
For example, starting in your home directory keeps `~/.agents/skills` and
`~/.octet/skills` user-installed, without an untrusted-project warning for those
same roots. This does not trust the workspace: distinct project roots, including
nested `.agents/skills` and the invocation's `.pi/skills`, remain gated. Root
symlinks are still rejected.

Later definitions replace earlier definitions of the same skill name; collisions
are recorded in discovery diagnostics. Discovery does not activate a skill.
The octet-native entrypoints remain `~/.octet/skills/*/SKILL.md`,
`.octet/skills/*/SKILL.md`, and managed
`~/.octet/extensions/*/skills/*/SKILL.md`.

These lookup locations do not promise full Agent Skills/Pi parser compatibility
or additional symlink support. Parser-specific shapes and symlink behavior for
those additional roots require their exact source contract.

## Reads and diagnostics

For octet-native resource roots, selected files, and directory entrypoints, the
existing resolver contract requires regular, non-symlink filesystem objects.
Parser reads use descriptor-bound no-follow opens and fixed byte limits:

| Kind | Maximum parser input |
| --- | ---: |
| Prompt | 512 KiB |
| Skill entrypoint | 256 KiB |
| Extension manifest selected by the product resource resolver | 256 KiB |

Prompt expansion, skill resource reads, extension protocol messages, and
session files have their own narrower purpose-specific limits after discovery.
The lower-level `ExtensionManifest::load` API has a separate 64 KiB default;
product discovery reads the selected manifest through the 256 KiB resolver
bound and then calls `ExtensionManifest::parse`.
Invalid UTF-8, invalid names, inaccessible roots, rejected links, oversized
files, parser failures, and precedence decisions become inspectable
diagnostics. One broken customization does not prevent the core binary from
starting.

## Reload

Each discovery pass produces an immutable generation snapshot. Consumers build
a complete replacement from the new snapshot and swap only after validation,
so an in-flight prompt never observes half of a reload.

- `/skills reload` refreshes the shared prompt/skill resource boundary.
- `/extensions reload` handshakes replacement processes under the default
  full-access policy when the independent process gate permits startup; safe mode
  leaves executable extension processes stopped.
- `/reload` performs full product resource discovery and rebuilds the active
  customization boundary.
