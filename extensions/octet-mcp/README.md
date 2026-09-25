# octet-mcp

**Distribution: 0.8.0.** This bundle requires exactly octet 0.8.0.
Use the [version-matched installation](../../docs/installation.md) and the
[0.8.0 release record](../../docs/releases/v0.8.0.md) for signed assets and
public-install evidence. Reviewed source checkouts and local archives remain
separate installation options.

Connect explicitly configured [MCP](https://modelcontextprotocol.io/) tool servers
to octet. Start with a local stdio server you have reviewed and installed
separately. The bridge never discovers or installs server software for you.

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

With [octet 0.8.0](../../docs/installation.md), Python 3.9+
on `PATH`, and verified matching published assets, the catalog path is:

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

Refresh rereads the tool catalog without relaunching. Restart replaces the
connection; stop removes its tools and closes it. `/mcp snapshot` returns the
same semantic state used by the TUI and Serve.

## Trust and limitations

A local server runs with your OS authority. Neither configuration nor tool
approval is a sandbox. Server trust does not approve every tool: only an exact,
uncontradicted JSON `readOnlyHint: true` gets read-only classification. Unknown
or destructive calls require host policy. The working-tree coding host binds
`mcp.tool.call` to the active owner, process generation, published tool identity,
and exact arguments: full access (`unsafe_host`) permits these calls, including
mutations. Controlled policies deny them and still block extension startup;
enabling an extension alone is not authorization. Unrecognized policy operations
remain denied. This adapter is a source change, not a claim about the published
`0.7.6` host, which denies these calls.
Calls are never automatically replayed after an ambiguous failure; cancellation
does not promise rollback.

Server descriptions, schemas, logs, and results are untrusted data. The server
gets only a small non-secret environment allowlist plus explicit `env` values,
not ambient provider tokens or dotenv files. Keep secrets out of labels and
arguments.

**Remote Streamable HTTP is blocked by default and unsafe for production,
privileged networks, or sensitive credentials.** Its process-owner-only
experimental switch is not a safety qualification. The [nine-defect remediation
record and remaining closure gates](REFERENCE.md#known-streamable-http-defects)
describe the tested safeguards and outstanding qualification work. The supported
remote surface stays deliberately narrow: one exact URL with no
redirects/proxies/cookies, one negotiated session, static extension-scoped
credentials only, and an optional permanent GET stream for a server that declares
a change notification. A remote descriptor may name one `OCTET_MCP_*` environment
variable that the bridge reads per request and never logs, echoes, or stores. The
stock runtime has no credential broker and no OAuth flow: `CredentialProvider`
adapters must be composed explicitly, and OAuth/browser authorization stays
policy-gated because the extension API has no host-brokered authorization
primitive. Legacy SSE authorization, resources, prompts, sampling, elicitation,
and ambient discovery are unsupported.

## Reference

The bundle requires exactly octet `0.8.0` and uses API `0.4`. The following
is a retained bundled-runtime contract, not a general SDK authoring guide.

- <a id="security-and-authority"></a>[Security and authority](REFERENCE.md#security-and-authority).
  - <a id="experimental-streamable-http-gate"></a>[Experimental Streamable HTTP gate](REFERENCE.md#experimental-streamable-http-gate).
  - <a id="known-streamable-http-defects"></a>[Known Streamable HTTP defects](REFERENCE.md#known-streamable-http-defects).
- <a id="requirements-and-installation"></a>[Requirements and installation](REFERENCE.md#requirements-and-installation).
- <a id="configuration"></a>[Configuration](REFERENCE.md#configuration): strict file validation and schema.
  - <a id="streamable-http-configuration"></a>[Streamable HTTP configuration](REFERENCE.md#streamable-http-configuration).
  - <a id="digest-pinned-trusted-project-configuration"></a>[Digest-pinned trusted project configuration](REFERENCE.md#digest-pinned-trusted-project-configuration).
  - <a id="enforced-default-bounds"></a>[Enforced default bounds](REFERENCE.md#enforced-default-bounds).
  - <a id="streamable-http-framing-and-recovery"></a>[Streamable HTTP framing and recovery](REFERENCE.md#streamable-http-framing-and-recovery), subject to the remaining closure gates.
- <a id="catalogs-calls-and-results"></a>[Catalogs, calls, and results](REFERENCE.md#catalogs-calls-and-results): epochs, cancellation, schemas, and media.
- <a id="lifecycle-health-and-recovery"></a>[Lifecycle, health, and recovery](REFERENCE.md#lifecycle-health-and-recovery).
- <a id="tui-and-serve-presentation"></a>[TUI and Serve presentation](REFERENCE.md#tui-and-serve-presentation): owner-fenced state, not a separate manager.
- <a id="tests"></a>[Tests](REFERENCE.md#tests): documented fixtures, not live remote-transport qualification.
