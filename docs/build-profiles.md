# Cargo build profiles

octet keeps Cargo's ordinary `dev`, `test`, and `release` behavior unchanged.
Two additive profiles make CI test artifacts and profiler builds explicit. The
root workspace declares them, because Cargo resolves profile definitions from the
active workspace root.

## `ci-test`

`ci-test` inherits Cargo's `test` profile, sets `debug = "limited"` and turns
off incremental compilation. CI backtraces keep filenames and module names,
without the full debug data of the test profile or an incremental cache that a
clean CI runner won't reuse.

CI's Rust test jobs use it. To reproduce them locally:

```sh
cargo test --workspace --all-targets --all-features --profile ci-test --locked
cargo test --workspace --doc --profile ci-test --locked
```

Leave out `--profile ci-test` for Cargo's normal local test behavior.

## `profiling`

`profiling` inherits the active workspace's `release` profile and its
optimization choices, and overrides only what analysis needs:

- `debug = "full"`: full source and variable debug info.
- `lto = "off"`: no cross-crate or local ThinLTO.
- `strip = "none"`: keep symbols.

Build a profiler-friendly octet binary:

```sh
cargo build --profile profiling --locked -p octet-coding-agent --bin octet
```

The binary is written to `target/profiling/octet` (or
`$CARGO_TARGET_DIR/profiling/octet` when that variable is set).

On platforms that split debug info, keep the companion debug files next to the
profiling binary when you hand it to a profiler.

## Measuring a profile change

Each profile has its own target subdirectory, so you can compare the same
command under both without deleting artifacts:

```sh
/usr/bin/time -p cargo test -p sexy-tui-rs --lib --locked
/usr/bin/time -p cargo test -p sexy-tui-rs --lib --profile ci-test --locked
du -sh target/debug target/ci-test
```

Run the same test selection in both, and record the result, elapsed time and
output size. Use a quiet machine or a dedicated target directory for a
reproducible result.
