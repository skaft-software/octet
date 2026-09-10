# octet-mcp

Connect explicitly configured [MCP](https://modelcontextprotocol.io/) servers
(tools and resources) to octet. Start with a local stdio server you have reviewed
and installed separately. The bridge never discovers or installs server software
for you.

**Unreleased source candidate:** the first-class MCP changes require the matching
host and extension from this PR stack, not the published `0.7.3` binary/bundle.
Remote HTTP remains experimental. [Qualification](QUALIFICATION.md) pins the
compatibility target and keeps unsupported features and unrun journeys explicit.

## Connect a local server

Put this in `~/.octet/mcp.json`, replacing the executable and working-directory
paths with your reviewed local paths. Protect the file with `chmod 600`.

```json
{
  "version": 1,
  "servers": {
    "local-example": {
      "transport": "stdio",
      "label": "Local example",
      "command": "/absolute/path/to/mcp-server",
      "args": ["--stdio"],
      "cwd": "/absolute/trusted/working-directory",
      "env": {},
      "enabled": true,
      "required": false
    }
  }
}
```

Python 3.9+ must be on `PATH`. For this candidate, build the matching source host
and select the reviewed checkout with `--extension-dir ./extensions`, as shown
below. These managed-install commands apply only after a matching host/bundle
release; installing the published `0.7.3` bundle does **not** supply this candidate:

```console
octet extension install octet-mcp
~/.octet/extensions/octet-mcp/octet-mcp --config ~/.octet/mcp.json --check-config
octet --enable-extension octet-mcp
```

A reviewed source checkout remains an alternative; add
`--extension-dir ./extensions` when launching from the repository root.
Installation and discovery are inert; the bridge stays disabled until explicitly
enabled. Default full access (`unsafe_host`) implicitly trusts the selected
extension without persisting a grant. `--trust-extension` and source-bound
`trusted_extensions` grants are optional and never enable it. `--safe-mode`
removes implicit trust and blocks startup even with explicit grants: executable
processes still require `unsafe_host`. This does not change MCP server trust or
tool-call policy. If the configuration file is absent, it stays healthy with zero
servers.

Inspect and manage the connection in any frontend:

```text
/mcp status
/mcp list
/mcp show local-example
/mcp refresh local-example
/mcp restart local-example
/mcp stop local-example
```

Refresh rereads the catalog without relaunching. Restart replaces the
connection; stop removes its tools and closes it. `/mcp snapshot` returns the
same semantic state used by the TUI and Serve. Status/list/snapshot/show are
observational; they never start a remote server.

## Explicit remote configuration (experimental)

After reviewing an endpoint, configure it normally, without a vendor-specific
adapter. This disabled example has no authorization:

```json
{
  "version": 1,
  "servers": {
    "remote-example": {
      "transport": "streamable-http",
      "url": "https://mcp.example.invalid/mcp",
      "enabled": false
    }
  }
}
```

Replace the URL and enable it only deliberately. Build/run the matching source
host from the repository root:

```console
cargo run -p octet-coding-agent --bin octet -- \
  --extension-dir ./extensions --enable-extension octet-mcp \
  --experimental-streamable-http-mcp
```

The process-owner flag is required; configuration, environment, project files,
server metadata and model requests cannot grant it. Enabled remotes wait for a
host-owned prompt or explicit lifecycle/setup action. Each isolated bridge
admits one host session/owner triple per generation; switching owners requires
host reload/replacement. Credentials, sessions, results and old handlers cannot
cross that fence.

Legacy initialization remains the default. A remote may explicitly select
`"protocolVersion": "2026-07-28"` for the stateless discovery/metadata/header
mode. This is not automatic protocol fallback or full modern MCP conformance.
See [the reference](REFERENCE.md#modern-http-mode) for supported boundaries.

For a bearer endpoint, add `"auth": {"type": "bearer", "credential": "reviewed_remote"}`.
For OAuth, use the [explicit issuer/public-client configuration](REFERENCE.md#private-authentication).
Then use **user commands**, never a model tool or token in chat:

```text
/mcp auth login remote-example
/mcp auth poll remote-example
/mcp auth status remote-example
/mcp auth cancel remote-example
/mcp auth logout remote-example
/mcp restart remote-example
```

Private setup requires the matching interactive TUI: the complete escaped context
must fit its viewport, and `OCTET_TUI_WRITE_LOG` must be unset. Serve/headless
private input is unavailable; never work around that by pasting secrets in chat.
Bearer login uses private input. OAuth first asks secret-free consent, then shows
its URL privately; open it manually and type `continue`, finish consent in your
browser, then poll. Login does not
replay a call or restart a connection. Logout removes this owner's local token
and stops the connection; it does not claim remote token revocation or secure
erasure. The POSIX store at `~/.octet/mcp-auth` is owner-private **plaintext**,
not an encrypted vault. No credential comes from model arguments, dotenv,
ambient provider tokens, or static HTTP headers.

Shopify is a separate interoperability journey, not built-in routing. Explicit
storefront `/api/mcp` and `/api/ucp/mcp` configurations are distinct; required UCP
profile metadata is ordinary tool input, never silently injected. Customer OAuth
is separate from public storefront access. Local storefront-shaped tests are
not evidence of a live Shopify store, Wix login, checkout or purchase.

## Trust and limitations

A local server runs with your OS authority. Neither configuration nor tool
approval is a sandbox. Server trust does not approve every tool: only an exact,
uncontradicted JSON `readOnlyHint: true` gets read-only classification. Unknown
or destructive calls require host policy. The matching source host offers
single-use, exact-call approvals for the isolated first-party bridge through a
trusted frontend; unavailable/headless, stale, cancelled, altered or oversized
requests remain denied. This generic call adapter does **not** implement #383's
app/tab/origin/fresh-observation automation policy. In particular, server hints
are not proof that a browser or desktop action is harmless.
Calls are never automatically replayed after an ambiguous failure; cancellation
does not promise rollback.

Server descriptions, schemas, logs, and results are untrusted data. The server
gets only a small non-secret environment allowlist plus explicit `env` values,
not ambient provider tokens or dotenv files. Keep secrets out of labels and
arguments.

**Remote Streamable HTTP remains blocked by default and unqualified for production,
privileged networks, or sensitive credentials.** The process-owner switch does
not waive [the retained defect/release ledger](REFERENCE.md#known-streamable-http-defects).
Legacy standalone SSE, prompts, sampling, roots, automatic installation/discovery,
and unrestricted JSON Schema are unsupported. Modern subscriptions and full
OAuth registration/enterprise parity remain gaps, not silently excluded scope.
Resources and private interactions use bounded supported subsets; see the
[reference](REFERENCE.md). Live authenticated/customer/store journeys remain
unrun and #179 remains open.

## Reference

An optional [MCP usage skill](skills/octet-mcp/SKILL.md) accompanies managed
installs. It remains inactive until selected; a source checkout can explicitly
supply `--skill-dir ./extensions/octet-mcp/skills`.

The bundle requires exactly octet `0.7.3`; its API remains `0.2`. The following
is a retained bundled-runtime contract, not a current SDK authoring guide.

- <a id="security-and-authority"></a>[Security and authority](REFERENCE.md#security-and-authority).
  - <a id="experimental-streamable-http-gate"></a>[Experimental Streamable HTTP gate](REFERENCE.md#experimental-streamable-http-gate).
  - <a id="known-streamable-http-defects"></a>[Known Streamable HTTP defects](REFERENCE.md#known-streamable-http-defects).
- <a id="requirements-and-installation"></a>[Requirements and installation](REFERENCE.md#requirements-and-installation).
- <a id="configuration"></a>[Configuration](REFERENCE.md#configuration): strict file validation and schema.
  - <a id="streamable-http-configuration"></a>[Streamable HTTP configuration](REFERENCE.md#streamable-http-configuration).
  - <a id="digest-pinned-trusted-project-configuration"></a>[Digest-pinned trusted project configuration](REFERENCE.md#digest-pinned-trusted-project-configuration).
  - <a id="enforced-default-bounds"></a>[Enforced default bounds](REFERENCE.md#enforced-default-bounds).
  - <a id="streamable-http-framing-and-recovery"></a>[Streamable HTTP framing and recovery](REFERENCE.md#streamable-http-framing-and-recovery), subject to the known defects.
- <a id="catalogs-calls-and-results"></a>[Catalogs, calls, and results](REFERENCE.md#catalogs-calls-and-results): epochs, cancellation, schemas, and media.
- <a id="lifecycle-health-and-recovery"></a>[Lifecycle, health, and recovery](REFERENCE.md#lifecycle-health-and-recovery).
- <a id="tui-and-serve-presentation"></a>[TUI and Serve presentation](REFERENCE.md#tui-and-serve-presentation): owner-fenced state, not a separate manager.
- <a id="tests"></a>[Tests](REFERENCE.md#tests): documented fixtures, not live remote-transport qualification.
