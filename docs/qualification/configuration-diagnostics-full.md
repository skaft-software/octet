# Configuration diagnostics qualification

Issue #313 targets a private module at
`crates/octet-coding-agent/src/cli/config_diagnostics.rs`, with minimal parent
wiring in `crates/octet-coding-agent/src/cli.rs`. The extraction must preserve
the existing configuration contract: layer precedence and persistence,
bounded/secure reads, missing-file handling, accepted aliases and ignored keys,
deterministic diagnostics, source and location reporting, warning/error
routing, and `OCTET_STRICT_CONFIG` behavior.

## Regression coverage

`crates/octet-coding-agent/tests/configuration_diagnostics_full.rs` exercises
the process boundary with an isolated home, workspace, session directory, and
cleared environment. It covers:

- the default unknown-key warning on stderr without provider credentials; and
- rejection caused by `OCTET_STRICT_CONFIG=true` without provider credentials.

Existing focused unit coverage remains responsible for the detailed parser,
merge, precedence, validation, and diagnostic cases. The production module
extraction and the corresponding focused-test move require edits to the
parent-owned `cli.rs` path.

## Verification status

No commands, formatting, builds, or tests were run, as required by the task
instructions. The parent owner should run the focused unit tests, the new
`configuration_diagnostics_full` integration test, and the relevant workspace
checks after wiring the private module.
