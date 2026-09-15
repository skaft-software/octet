# LEDGER rows-api — Phase 3b extensions API + 3c migration (12 issues)

Format: `#NNN | verdict | evidence | missing`
Policy: `cargo check --workspace --all-targets` green (not re-verified); no test was run by this worker.

#257 | implemented | extensions/octet-pi-compat/conformance.py:331, extensions/octet-pi-compat/real_runtime.py:417, extensions/octet-pi-compat/tests/test_conformance.py:63 | —
#261 | partial | scripts/bench-pi-runtime.py:1, docs/benchmarks/pi-runtime-evidence.md:1, docs/benchmarks/pi-runtime-evidence.md:3 | harness measures resource samples + per-profile p95 latency but declares release decision always `hold`; no v0.8 resource/latency evidence artifact published anywhere in tree
#262 | implemented | crates/octet-agent/src/extension_api_v03.rs:85, crates/octet-coding-agent/src/migrate/migration_import.rs:350, crates/octet-coding-agent/src/migrate/migration_import.rs:1028 | — (typed detect/import adapter contract + MigratedSetup normalization; test targets crates/octet-migrate-types/tests/schemas.rs, crates/octet-coding-agent/tests/migration_import.rs)
#263 | implemented | crates/octet-agent/src/extension.rs:157, crates/octet-agent/src/extension.rs:826, crates/octet-agent/tests/agent_run.rs:8166 | —
#264 | partial | crates/octet-agent/src/tool.rs:197, crates/octet-agent/src/extension_process.rs:14448, sdk/python/octet_extension/extension.py:1161 | no test target exercises the bounded progress-decoration path (`progress_decoration` appears in code only, never in crates tests/fixtures or SDK tests)
#265 | partial | crates/octet-agent/src/extension.rs:229, crates/octet-agent/src/agent.rs:3209, crates/octet-agent/src/session.rs:251 | no test target covers the `before_persistence` path (no test in crates/*/tests or SDK tests references extension_metadata/persistence_metadata)
#268 | implemented | crates/octet-agent/src/extension_process.rs:5649, crates/octet-coding-agent/src/extensions.rs:3147, extensions/octet-pi-compat/tests/test_bridge_protocol.py:425 | —
#269 | implemented | crates/octet-agent/src/extension_provider.rs:195, crates/octet-agent/src/extension_provider.rs:265, crates/octet-agent/src/extension_provider.rs:822 | — (registration + catalog lifecycle proven by `crates/octet-agent` --lib tests)
#270 | implemented | crates/octet-agent/src/extension_process.rs:5236, crates/octet-agent/src/extension_process.rs:14013, crates/octet-agent/src/extension_process.rs:16919 | — (bounded buffer/idle/deadline config + cancel, proven by `crates/octet-agent` --lib tests)
#156 | partial | crates/octet-coding-agent/src/migrate/migration_import.rs:326, crates/octet-coding-agent/src/migrate.rs:102, crates/octet-coding-agent/src/migrate/migration_import.rs:491 | Pi adapter ships as a hidden built-in (`octet migrate adapter pi`, self-identifies as octet-import-pi); no `extensions/octet-import-pi/` package (manifest/tests/README) unlike octet-import-aider/-cline
#157 | implemented | crates/octet-coding-agent/src/migrate/migration_import.rs:154, crates/octet-coding-agent/src/migrate/migration_import.rs:1798, crates/octet-coding-agent/tests/migration_host_full.rs:113 | — (real symbols are apply_ingestion_plan/create_backup/restore_backup + migration lock, not `apply_migrated_setup`)
#279 | implemented | crates/octet-coding-agent/src/migrate.rs:61, crates/octet-coding-agent/src/migrate/migration_import.rs:52, crates/octet-coding-agent/tests/migration_import.rs:7 | —

NOTES:
- #156 is the only missing artifact in the migration pair: aider/cline import extensions exist as packages, the Pi one is a built-in hidden adapter.
- #264 and #265 are code-complete (Rust host + SDK client) but have zero test references in-tree; verdict is partial for the missing proving test target, not for missing code.
- #261 has a harness (`scripts/bench-pi-runtime.py`) that hard-holds the release decision; no v0.8 resource/latency evidence artifact exists anywhere in the tree.
- Every cited file was opened; no test/cargo command was run by this row.
