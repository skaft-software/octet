# octet-mcp

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

With [octet 0.7.0 installed](../../docs/installation.md) and Python 3.9+ on
`PATH`, install the signed public bundle and validate your configuration:

```console
octet extension install octet-mcp
~/.octet/extensions/octet-mcp/octet-mcp --config ~/.octet/mcp.json --check-config
octet --enable-extension octet-mcp --trust-extension octet-mcp
```

A reviewed source checkout remains an alternative; add
`--extension-dir ./extensions` when launching from the repository root.
Installation and discovery are inert; enablement and exact executable trust are
separate. The bridge runs only under full-access policy, never in `--safe-mode`.
If the configuration file is absent, it stays healthy with zero servers.

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
or destructive calls require host policy. The octet `0.7.0` coding product does
not issue approvals for those calls, so they fail closed with a tool error.
Calls are never automatically replayed after an ambiguous failure; cancellation
does not promise rollback.

Server descriptions, schemas, logs, and results are untrusted data. The server
gets only a small non-secret environment allowlist plus explicit `env` values,
not ambient provider tokens or dotenv files. Keep secrets out of labels and
arguments.

**Remote Streamable HTTP is blocked by default and unsafe for production,
privileged networks, or sensitive credentials.** Its process-owner-only
experimental switch does not resolve the [nine known defects](REFERENCE.md#known-streamable-http-defects).
The stock runtime has no remote credential provider. Legacy SSE, OAuth/browser
authorization, resources, prompts, sampling, elicitation, and ambient discovery
are unsupported.

## Reference

The bundle requires exactly octet `0.7.0`; its API remains `0.2`. The following
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
