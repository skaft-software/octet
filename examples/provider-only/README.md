# Provider-only consumer fixture

This independent, non-published crate depends on **`octet-ai` only** among Octet
packages. It never reads a real credential store or calls a live provider.

From the repository root:

```sh
cargo run --manifest-path examples/provider-only/Cargo.toml --locked
cargo test --manifest-path examples/provider-only/Cargo.toml --locked
sh examples/provider-only/check-dependencies.sh
```

The [source](src/main.rs) seeds synthetic existing-format credentials under a
private temporary root. It loads an account-scoped model cache, resolves the
exact Codex route, and redirects only that fixture model's destination/transport
to a local HTTP fake. Static headers, dynamic authentication, protocol/runtime,
compression, capabilities and pricing come from the shared implementation.
No API key, frontend, agent loop, terminal, session or extension host is involved.

Tests cover completion, refusal, truncation, incomplete/missing terminals,
ambiguous disconnect, timeout/cancellation with no replay, credential-safe
errors, redirect refusal, invalid destination rejection and local-only expired
credential/cache inspection. They are not live-provider qualification. In
particular, the declared Codex route does **not** enforce an output-token cap;
the fixture checks that `effective_output_token_cap` reports this limitation.

Real embedding uses the shared login or an explicitly selected existing store,
not the fixture's synthetic credential seeding or endpoint override. See the
[API and migration notes](../../docs/provider-embedding.md).
