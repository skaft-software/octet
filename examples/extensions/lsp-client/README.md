# lsp-client

**Legacy implementation reference**, not an API `0.3` quickstart. Its
`before_prompt` hook is a legacy request-path interface, not a current API
`0.3` cleanup hook. See [current authoring](../../../docs/extensions.md) and
the [legacy Python runtime](../../../sdk/python/legacy-runtime.md). Do not retag
its manifest.

One read-only `code_intelligence` model tool supports `definition`, `references`,
`hover`, and pull `diagnostics`. The `before_prompt` hook injects new
language-server diagnostics once per change.

## Scope and boundaries

- Text-first: `read`, search, and build/test commands remain fallbacks.
  Unavailable states are typed, bounded results, never product failures.
- No hidden mutation: `didOpen`/`didChange` keep server document state current;
  server-proposed edits are never applied.
- Servers are never downloaded or installed; missing binaries give typed
  unavailable results.
- Executable startup still needs independent enablement, trust, and full-access
  admission; `--safe-mode` keeps it stopped. Use separate OS isolation for trusted
  processes; capability declarations are not a sandbox.

## Supported servers

File suffix configuration lives in `extension.py` (`DEFAULT_SERVERS`):

| Suffix | Server |
| --- | --- |
| `.rs` | `rust-analyzer` |
| `.py` | `pyright-langserver --stdio` |
| `.ts/.tsx/.js/.jsx` | `typescript-language-server --stdio` |

The server binary must already be on `PATH`.

## Behavior

- Lazy start: first query for a matching suffix starts its server.
- Bounded: each request has a deadline. Restarts cap at three consecutive failures
  or ten lifetime starts/server. Results cap at ten definitions, 100 references,
  20 diagnostics/file, and 2 KB hover text.
- Document sync: re-read files before every query; changes made by `edit`, `write`,
  or shell tools are sent with `didChange`, avoiding stale diagnostics.
- Diagnostics injection: only previously uninjected diagnostics are contributed.
  Clean files are forgotten so regressions re-report; total injection is capped
  per turn.
- Crash handling: kill a dead server's process group and restart within the same
  bounds. In-flight requests fail with typed unavailable results.

## Tests

```console
python3 examples/extensions/lsp-client/test_extension.py
```

The documented suite uses a fake LSP server over real stdio, covering navigation,
document resync, diagnostics delivery/deduplication, and dead/silent server,
unconfigured suffix, and missing-file failures.

Development tracking: [project board](https://github.com/orgs/skaft-software/projects/5).
