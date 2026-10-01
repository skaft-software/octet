# Migrating from Pi

Inspect your Pi setup before importing anything:

```console
octet migrate pi --dry-run
```

The scanner reads local files only. It doesn't run package code, start a model
or change either setup. It always runs dry, even without `--dry-run`, so it
isn't an apply command or an extension-compatibility promise. Inventory,
portable setup import and restore, and [native provider support](providers.md)
are separate capabilities. **Pi extension execution is not supported** in the
local release candidate: the former bridge and its `octet pi` command family
have been removed.

## Current command

The scanner reads `~/.pi/agent/settings.json` and the selected project's
`.pi/settings.json`. `PI_CODING_AGENT_DIR` or `--pi-home` selects another user
directory, and `--project` selects another project:

```console
octet migrate pi --dry-run --project /path/to/project
octet migrate pi --dry-run --json > pi-migration.json
```

It exits before normal octet config, provider discovery, session or extension
startup and model bootstrap, so it uses no model tokens. Read the [report
classifications](#classification) before treating anything as compatible.

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
package code or an adapter you pick. octet owns the destinations and does the
writing. The adapter rejects a selected source directory that's a symlink, and
the CLI keeps that boundary for explicit, environment and default source paths.

| Data | What import does |
| --- | --- |
| Model selection | Selects a Pi provider and API-model pair only if it matches exactly one entry in octet's built-in catalog, then saves the canonical catalog ID. Custom, unknown and ambiguous pairs are skipped, not guessed. |
| Skills | Copies them to `~/.octet/skills/` with `disable-model-invocation: true` added. Review before enabling. |
| Local stdio MCP declarations | Adds them to `~/.octet/mcp.json` with `enabled: false` and `required: false`. Review before enabling. |

Credentials, MCP environment values, headers, working directories and Pi
permission decisions are **never copied**. Unsupported models and transports are
reported as skipped, with text details and JSON `model_diagnostics`. Import
never writes the Pi setup, uses the network, starts an imported MCP server or
extension, or calls a model.

octet tracks the hashes it owns in `~/.octet/migrations/pi-state.json`. A
changed destination is a conflict, which needs interactive confirmation or
`--yes`. `--dry-run` checks the same inputs without writing. Before changing a
destination, import makes a private backup under `~/.octet/backups/migrate/` and
prints its path.

### Restore an import

Restore normally needs the current destination to still match the import:

```console
octet migrate restore ~/.octet/backups/migrate/IMPORT-DIRECTORY
# Explicitly overwrite a destination changed after import:
octet migrate restore ~/.octet/backups/migrate/IMPORT-DIRECTORY --yes
```

Review the backup and your later edits before using the overwrite option.
Restore applies only to imported setup data, not to extension execution.

<a id="plan-preflight-and-publish-a-compatible-extension"></a>

## Pi extension execution is not supported

The former install, plan, preflight, publish, list and rollback workflow is
gone. Don't use inventory results to run Pi package code, enable an extension
automatically or infer trust. Review portable resources and explicitly port any
tools you need to [octet's bounded extension API](extensions.md).

## Scanner reference

The scanner reads configured local, managed npm and managed git packages without
installing missing ones, and reports an inventory. Malformed, missing,
oversized, linked or unsupported input becomes a diagnostic, and one bad package
doesn't stop the rest.

<details>
<summary>What the scanner does</summary>

It applies Pi manifests, conventional resource directories, package filters and
top-level resource overrides, records installed names and versions, and hashes
bounded source and config separately from locks. It parses JavaScript,
TypeScript and TSX with tree-sitter, follows bounded relative imports inside
each package, and inventories Pi events, registrations, UI calls, mutations and
runtime imports. Filesystem, process, network, secret, native-module and
dynamic-import signals are a conservative inventory, not proof of runtime
effects.

</details>

### Classification

| Path | Meaning |
| --- | --- |
| `direct` | Pi skills or Markdown prompts that have a deterministic octet resource path. The scanner doesn't copy them. |
| `replace` | Reserved for an exact package, version and source-hash native replacement recipe. No replacement recipes ship. |
| `bridge` | A retained scanner classification against its pinned historical profile. There's no shipping Pi bridge, so review it for a native port, not for unchanged execution. |
| `native_port` | Uses a known Pi `0.84.4` mutation or registration that needs an explicitly scoped native port or a redesign. No future host primitive is promised. |
| `manual` | Arbitrary Pi TUI and editor components, custom providers, or deep session and compaction internals need redesign. Pi JSON themes also need manual conversion to octet's different semantic schema. |
| `blocked` | Couldn't be fully resolved, read or parsed, or uses names outside the pinned Pi `0.84.4` public profile. |

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

Package entries include the source and scope, resolved root, name and version,
source and lock hashes, resources and Pi-filter enablement, extension analyses,
file, byte and node counts, unresolved imports and diagnostics. A hash is left
out if its complete input can't be read within bounds. Package identity and
hashes are inventory evidence, not execution authority.

## Safety and bounds

The scanner never runs, imports, installs, trusts or starts anything, never
sends data anywhere, never changes either setup, and never reads Pi credential
stores. Static scanning isn't a sandbox or permission to run a package. An
extension you've explicitly ported and enabled runs with your OS authority. See
[executable extensions](extensions.md#kernel-boundary) and
[security](../SECURITY.md).

<details>
<summary>Scanner limits</summary>

The scanner doesn't execute or import extensions, run npm lifecycle scripts,
install packages, trust or start extensions, send data anywhere, copy, rewrite
or delete either setup, or read Pi credential stores. Everything it reads has a
fixed limit, uses no-follow regular-file reads, and rejects linked package roots
and resources. `--npm-root` only adds an explicit legacy `node_modules` search
root. It never runs a configured Pi `npmCommand`.

</details>

## Deliberate compatibility boundary

Portable import and inventory don't run Pi extensions, import Pi sessions, or
give you Pi UI, editor or session-mutation parity. Pi themes and executable
resources need explicit review and, where useful, redesign for octet's host
boundaries. There's no automatic model-assisted fallback, dependency installer
or whole-setup conversion. Browse, MCP, web search and subagents are bounded
octet integrations, not a Pi component ABI.

Native provider routes are independent of extension execution. See
[providers](providers.md) for current routes and limits. A historical comparison
ledger is evidence about a tested route, not a promise to complete Pi's
inventory.

## Project and earlier section links

Future migration work is tracked in the
[project](https://github.com/orgs/skaft-software/projects/5), not promised by
these commands. Model-assisted porting isn't an automatic fallback. No current
scanner invocation silently starts model use.

- <a id="migration-architecture"></a>[Migration
  architecture](#deliberate-compatibility-boundary).
- <a id="deterministic-scannercompiler"></a>[Deterministic
  scanner/compiler](#scanner-reference).
- <a id="compatibility-process"></a>[Removed compatibility
  process](#pi-extension-execution-is-not-supported).
- <a id="exact-recipes"></a>[Exact
  recipes](https://github.com/orgs/skaft-software/projects/5): none ship. See
  [classification](#classification).
- <a id="agentic-fallback"></a>[Agentic
  fallback](https://github.com/orgs/skaft-software/projects/5): not an
  implemented automatic migration path.
- <a id="product-promise"></a>[Current
  scope](#deliberate-compatibility-boundary) and
  [project](https://github.com/orgs/skaft-software/projects/5).
