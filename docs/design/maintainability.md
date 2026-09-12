# Maintainability plan (BigRefactor)

Make octet easier for a human to understand, change, and review. This is
engineering work, not a prerequisite for using the product or shipping unrelated
fixes. The [public roadmap](https://github.com/skaft-software/octet/blob/main/ROADMAP.md) selects product outcomes; the
[qualification tracker](https://github.com/skaft-software/octet/issues/19) retains
existing repair work. Historical phase plans are not a queue to reopen wholesale.

## What should improve

- A maintainer can find the owner of a behavior without following a chain of
  forwarding modules or reading unrelated subsystems.
- Configuration, provider transport, run policy, persistence, and presentation
  retain explicit boundaries. Shared state and lifecycle ordering stay visible.
- A small behavior change has a small review surface and a nearby regression test.
- Documentation names the real implementation and its limits, not an intended
  architecture presented as complete.

File size alone is not the problem. Moving code without improving ownership or
reviewability is not acceptance. Do not add a framework, generic builder, catch-all
helper module, compatibility layer, or public API merely to make a split compile.

## First change: configuration diagnostics

Use the existing [configuration issue #313](https://github.com/skaft-software/octet/issues/313)
for a separately reviewable, behavior-preserving slice.

`crates/octet-coding-agent/src/cli.rs` currently combines flag parsing, configuration
schema/merging, atomic persistence, and diagnostic/loading logic. The contiguous
block beginning at `ConfigSourceKind` and ending at `read_layer` is a bounded first
extraction. Its [documented contract](config-diagnostics.md) already identifies
source, trust, precedence, compatibility, warning, and strict-mode behavior.

1. Add a credential-free subprocess regression for emitted warning text and
   `OCTET_STRICT_CONFIG` rejection. Existing unit tests cover diagnostic values and
   CLI strict mode, but the default-warning test does not capture stderr.
2. Move that diagnostic/loading implementation and its focused tests into a
   private `cli/config_diagnostics.rs` module. Expose only what the parent needs.
3. Leave argument parsing, configuration merging/precedence, persistence,
   environment handling, and supported settings unchanged.
4. Update the existing diagnostics document with the new owner path.

**Acceptance:** diagnostic behavior and tests can be reviewed without navigating
extension-flag parsing or atomic persistence. Preserve secure bounded reads,
missing-file behavior, accepted/ignored keys, source/location text, deterministic
ordering, warning output, and strict rejection. This extraction does not remove
the duplicated setting/key inventory; do not claim that it does.

## How subsequent changes are selected

Finish and review the first slice before selecting another. Name the concrete
maintenance friction, current owner, proposed boundary, preserved behavior, and
regression evidence on the owning issue. Prefer frequently changed or duplicated
policy/lifecycle code over mechanical file moves. A later review can compare
initial Agent construction with idle rebuild, without assuming their different
session and extension teardown paths can be merged.

Land one coherent change at a time. Keep production code, tests, and ownership
documentation together. A reviewer should be able to explain the resulting flow
without reconstructing an agent conversation or an obsolete phase plan.

## Verification

Before moving code, establish the existing regression baseline on the exact
candidate. For the first slice, run the focused CLI tests and then the affected
crate with the existing `ci-test` profile. Follow the full
[contribution checks](../../CONTRIBUTING.md#tests) before review, including format,
workspace checks/tests, lint, security/dependency checks, and `git diff --check`.
Record exact commands, candidate identity, failures, and unrun checks; existing
tests and this plan are not evidence that a refactor has been implemented or
qualified. Keep externally visible changes in separate patches.
