# Migrating from Pi

Inspect your Pi setup before importing anything:

```console
octet migrate pi --dry-run
```

This scanner reads local files without running package code, starting a model,
or changing either setup. It always runs dry, even without `--dry-run`; it is
not an apply command or an extension-compatibility promise. Inventory, portable
setup import/restore, and [native provider support](providers.md) are separate
capabilities. **Pi extension execution is not supported** in the local RC; the
former bridge and its `octet pi` command family have been removed.

## Current command

The scanner reads `~/.pi/agent/settings.json` and the selected project's
`.pi/settings.json`. `PI_CODING_AGENT_DIR` or `--pi-home` selects another user
directory; `--project` selects another project:

```console
octet migrate pi --dry-run --project /path/to/project
octet migrate pi --dry-run --json > pi-migration.json
```

It exits before normal octet configuration, provider discovery, session or
extension startup, and model bootstrap, so it uses no model tokens. See the
[report classifications](#classification) before treating anything as compatible.

## Import portable setup data

Import is a separate, opt-in command. Preview it first:

```console
octet migrate import pi --dry-run
octet migrate import pi --source /path/to/pi/agent --dry-run --json
octet migrate import pi --source /path/to/pi/agent
# Explicitly accept destination conflicts:
octet migrate import pi --source /path/to/pi/agent --yes
```

Without `--source`, import checks `PI_CODING_AGENT_DIR`, then the standard Pi
agent locations. It uses octet's built-in read-only API `0.3` adapter, not Pi
package code or a user-selected adapter command. The host owns destinations and
writes. A selected source-directory symlink is rejected by the adapter; the CLI
preserves that boundary for explicit, environment and default source paths.

The optional [octet-import-pi source package](../extensions/octet-import-pi/README.md)
provides a thin API `0.4` process entrypoint to the same implementation. It requires
a matching octet installation, introduces no alternate parser or ingestion path,
and is neither required by nor able to override `octet migrate import pi`.

| Portable data | Import behavior |
| --- | --- |
| Model selection | Selects a Pi provider/API-model pair only if it has exactly one match in octet's built-in catalog, then persists the canonical catalog ID. Custom, unknown, and ambiguous pairs are skipped, not guessed. |
| Skills | Copies to `~/.octet/skills/` with host-authored `disable-model-invocation: true` frontmatter. Review before explicitly enabling. |
| Local stdio MCP declarations | Adds to `~/.octet/mcp.json` with `enabled: false` and `required: false`. Review before explicitly enabling. |

Credentials, MCP environment values, headers, working directories, and Pi
permission decisions are **never copied**. Unsupported models/transports are
reported as skipped; model skips have bounded text details and JSON
`model_diagnostics`. Import never writes the Pi setup, uses a network service,
starts an imported MCP server or extension, or invokes a model.

Owned hashes are tracked in `~/.octet/migrations/pi-state.json`. A changed
destination is a conflict requiring interactive confirmation or `--yes`.
`--dry-run` validates the same inputs without writing. Before changing a
destination, import creates a private backup under `~/.octet/backups/migrate/`
and prints its path.

### Restore an import

Restore normally requires the current destination to still match the import:

```console
octet migrate restore ~/.octet/backups/migrate/IMPORT-DIRECTORY
# Explicitly overwrite a destination changed after import:
octet migrate restore ~/.octet/backups/migrate/IMPORT-DIRECTORY --yes
```

Review the backup and later local edits before using the overwrite option. This
restore applies only to imported setup data, not extension execution.

<a id="plan-preflight-and-publish-a-compatible-extension"></a>

## Pi extension execution is not supported

The former install/plan/preflight/publish/list/rollback workflow is no longer
available. Do not use inventory results to execute Pi package code, auto-enable
an extension, or infer trust. Review portable resources and explicitly port any
needed tools to [octet's bounded extension API](extensions.md).

## Scanner reference

The scanner resolves configured local, managed npm, and managed git packages
without installing missing ones. It applies Pi manifests, conventional resource
directories, package filters, and top-level resource overrides; records installed
names/versions; and hashes bounded source/configuration separately from locks.
It parses JavaScript, TypeScript, and TSX with tree-sitter, follows bounded
relative imports inside each package, and inventories Pi events, registrations,
UI calls, mutations, and runtime imports. Filesystem, process, network, secret,
native-module, and dynamic-import signals are conservative inventory, not proof
of runtime effects. Malformed, missing, oversized, linked, or unsupported input
becomes a diagnostic; one bad package does not stop the rest.

### Classification

| Path | Meaning |
| --- | --- |
| `direct` | Pi skills or Markdown prompts have a deterministic octet resource path; the scanner does not copy them. |
| `replace` | Reserved for an exact package/version/source-hash native replacement recipe. No replacement recipes ship. |
| `bridge` | Retained scanner classification against its pinned historical profile. There is no shipping Pi bridge; review for a native port, not unchanged execution. |
| `native_port` | Uses a known Pi `0.84.4` mutation/registration requiring an explicitly scoped native port or redesign; no future host primitive is promised. |
| `manual` | Arbitrary Pi TUI/editor components, custom providers, or deep session/compaction internals need redesign. Pi JSON themes also need manual conversion to octet's different semantic schema. |
| `blocked` | Could not resolve/read/parse completely, or uses names outside the pinned Pi `0.84.4` public profile. |

Unsupported calls are never silently classified as no-ops.

### Machine-readable report

`--json` emits schema version `1`, with explicit safety fields:

```json
{
  "schema_version": 1,
  "source": "pi",
  "mode": "dry_run",
  "model_usage": "disabled",
  "package_code_executed": false
}
```

Package entries include configured source/scope, resolved root, name/version,
source and lock hashes, discovered resources and Pi-filter enablement, extension
analyses, analyzed file/byte/node counts, unresolved internal imports, and
diagnostics. Source hashes cover the manifest, discovered resources, reachable
relative modules, and bounded source/configuration; lock hashes cover supported
npm/pnpm/Yarn lockfiles. A hash is omitted if its complete selected input cannot
be read within bounds. Package identity and hashes are inventory evidence, not execution authority.

## Safety and bounds

The scanner does not execute/import extensions, run npm lifecycle scripts,
install packages, trust/start executable extensions, send data to a model/network,
copy/rewrite/delete either setup, or read Pi authentication/model credential
stores. Settings, manifests, source, locks, resources, relative import closure,
and aggregate hashing have fixed limits. Selected files use descriptor-bound,
no-follow regular-file reads; linked package roots/resources are rejected.
`--npm-root` only adds an explicit legacy `node_modules` search root. The scanner
never executes a configured Pi `npmCommand`.

Static scanning is not a sandbox or permission to execute a package. An explicitly
ported and enabled octet extension runs with your OS authority. See
[executable extensions](extensions.md#kernel-boundary) and [security](../SECURITY.md).

## Deliberate compatibility boundary

Portable import and inventory do not run Pi extensions, import Pi sessions, or
provide Pi UI/editor/session mutation parity. Pi themes and executable resources
need explicit review and, where useful, redesign for octet's host boundaries.
There is no automatic model-assisted fallback, dependency installer, or whole-
setup conversion. Browse, MCP, web search, and subagents are bounded octet
integrations, not a Pi component ABI.

Native provider routes are independent of extension execution. Consult
[providers](providers.md) for current routes and limits.
