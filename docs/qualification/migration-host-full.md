# #279 migration-host-full qualification

**Status: source-only, uncommitted; host qualification is pending.** No Cargo,
rustc, test, build, formatting, or git command was run in this lane. This record
must not be read as installed-binary, live-provider, or Pi-parity acceptance.

## Candidate and boundary

This candidate covers the typed, host-owned Pi ingestion path. The source
adapter is the built-in API `0.3` process; it does not execute Pi packages or
copy adapter-owned credentials. The host validates the typed result, plans
changes against its own destination files, retains a private backup before a
write, and owns restore and rollback.

Relevant source evidence inspected:

- `crates/octet-coding-agent/src/migrate/migration_import.rs` — normalization,
  conflict approval, private destination persistence, state hashes, backups,
  restore, and rollback.
- `crates/octet-coding-agent/src/hydrate.rs` — bounded session/tool-result
  hydration and deterministic image placeholders.
- `crates/octet-migrate-types/src/lib.rs` — validated wire decoding.
- `crates/octet-agent/src/extension_api_v03.rs` and
  `crates/octet-coding-agent/src/extensions.rs` — API `0.3` negotiation and
  migration adapter process boundaries.
- `docs/pi-migration.md` — user-facing import, review, credential, and restore
  contract.

The black-box fixture is
`crates/octet-coding-agent/tests/migration_host_full.rs`. It uses an explicit
source directory, a temporary destination `HOME`, and an environment-cleared
child process. Its source fixture contains a canonical model, a skill, and a
local stdio MCP declaration with deliberately secret environment/header values.

## Deterministic acceptance matrix

| Acceptance cell | Fixture evidence | Qualification boundary |
| --- | --- | --- |
| Typed import preview, apply, host-authored disabled outputs, credential safety, and source immutability | `typed_import_preview_apply_and_idempotent_rerun_are_host_owned` | Local temporary files only; Cargo execution is pending. |
| Dry-run non-mutation and no-match behavior | The same test's preview; `no_match_is_reported_without_destination_artifacts` | Explicit source directory only; default-location discovery is not claimed here. |
| Idempotent rerun | `typed_import_preview_apply_and_idempotent_rerun_are_host_owned` | Repeated local import of unchanged fixture data; no endurance claim. |
| Conflict review and non-interactive cancellation | `changed_imported_data_requires_explicit_noninteractive_confirmation` | The child has no TTY, so refusal with the required `--yes` guidance is tested; an interactive physical-terminal prompt is not. |
| Private backup and verified restore | The two import/restore assertions in the typed-import test and the forced/non-forced restore assertions in the conflict test | Local backup path and hash checks only; no user backup migration or filesystem fault campaign. |
| Adapter failure containment | `adapter_rejection_is_explicit_and_leaves_destination_untouched` | A symlink source is rejected by the adapter as a deterministic adapter failure; an OS-level process crash is not claimed. |
| Automatic mid-apply rollback | Host rollback paths were inspected in `migration_import.rs`; no deterministic CLI fault injection was added | Pending focused unit/integration execution and any fault-injection qualification chosen by the integration owner. |
| Hydration bounds and image placeholders | Existing bounded hydration source/tests inspected | Not duplicated in this host-import fixture; no large-media or live-session claim. |

The fixture intentionally asserts that MCP `env` and `headers` are absent from
the destination and that their values do not appear in destination bytes. It does
not assert that source secrets are deleted or sanitized; the source remains
read-only and unchanged.

## CLI/lib.rs handoff

The CLI/lib.rs-owned lane must verify or retain these wiring requirements; this
lane made no changes to those files:

1. `TopLevelCommand::Migrate` must dispatch before normal configuration,
   provider/model bootstrap, session startup, or extension startup.
2. The public paths must remain reachable exactly as the import contract uses
   them: `octet migrate import pi [--source DIR] [--dry-run] [--json] [--yes]`
   and `octet migrate restore BACKUP [--yes]`.
3. Import and restore must preserve the existing isolated destination-home,
   trust/scope, cancellation, credential, backup, and rollback behavior; no
   CLI convenience path may bypass the host-owned plan.
4. The hidden `migrate adapter pi` entrypoint must remain available to the host
   adapter client but must not become an arbitrary user-selected adapter or a
   package-execution path.
5. The handoff owner should run the focused black-box test below after wiring and
   inspect the resulting diff for changes outside the agreed CLI/lib.rs paths.

## Verification record

Observed in this source-only lane:

- Relevant migration, adapter, protocol, hydration, persistence, backup, restore,
  and existing unit-test sources were inspected.
- The missing black-box fixture and this qualification record were authored.

Not run, because execution was prohibited for this owner lane:

```text
cargo test --locked -p octet-coding-agent --test migration_host_full -- --nocapture
cargo test --locked -p octet-coding-agent --test migration_import -- --nocapture
cargo test --locked -p octet-coding-agent --lib migration_import -- --nocapture
cargo fmt --all -- --check
```

The presence of the fixture is not a passing result. The integration owner must
run the commands against the exact integrated candidate, record failures
independently, and retain this source-only limitation until then.

## Explicit limits

- No installed binary, physical terminal, live provider, network service,
  package execution, credentialed source, or endurance evidence was used.
- No full Pi configuration/session compatibility claim is made.
- No CLI/lib.rs integration change is claimed by this lane.
- Automatic rollback is preserved by source inspection but remains an execution
  gate for the integrated candidate.
