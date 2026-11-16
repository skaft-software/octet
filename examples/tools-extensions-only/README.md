# Tools and extensions without orchestration

This non-published, independent workspace imports exactly Ultra's requested
`octet-agent` tool and extension-host surface. Its dependency disables defaults
and selects `tools,extensions`; it does not depend on the frontend or renderer.

```sh
cargo check -p octet-agent --no-default-features --features tools,extensions
python3 examples/tools-extensions-only/check-dependencies.py
```

The script checks Cargo's **resolved** features (including the compiler artifact),
rejects frontend/renderer dependencies, and inspects rustc's dep-info for omitted
implementation modules, including both provider-context and session-leaf drivers.
It then runs three runtime tests and nineteen independent compile-fail import
probes. Tests execute all four built-ins with caller-owned context and effect
admission, deliver tool progress, and load/call/shut down an API 0.4 subprocess.
The peer asserts that agent-session and native history services are not offered.
It uses Python 3 on Unix, with no Pi adapter, provider, credentials or network.
The shell call is a local `printf`; files stay under a disposable directory.

Run with isolated `HOME` and `TMPDIR`, keeping the real `CARGO_HOME` and
`RUSTUP_HOME` so installed toolchains resolve. Leave any caller-provided
`CARGO_TARGET_DIR` unchanged. Direct tests are also available:

```sh
cargo test --manifest-path examples/tools-extensions-only/Cargo.toml --locked
```

Cargo features are additive. The independent workspace prevents the coding
frontend's default-enabled agent dependency from masking this boundary.
See [embedding](../../docs/provider-embedding.md#ultras-downstream-boundary) and
the [dependency map and measurements](../../docs/maintainers/agent-feature-boundary.md).
