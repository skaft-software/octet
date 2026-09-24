# octet-mcp reference

**Distribution version: 0.7.6.** Catalog commands below require version-matched
published assets. Source checkouts and local archives require exactly octet 0.7.6.
See the [release record](../../docs/releases/v0.7.6.md) for publication and
installation evidence.

[Usage guide](README.md). This is the bundled API `0.2` implementation contract,
not a current extension-authoring example. Distribution `0.7.6` requires exactly
octet `0.7.6`; these version numbers are independent.

One resident extension process owns every explicitly configured
[Model Context Protocol](https://modelcontextprotocol.io/) server session and
maps its live tool catalog to transactional `tools/register` and
`tools/unregister` calls.

```text
octet <- API 0.2 JSON-RPC -> octet-mcp <- MCP JSON-RPC stdio -> local servers
                                 \-> MCP Streamable HTTP -> explicit remote endpoint
```

Local stdio is the normal transport. Streamable HTTP is blocked-by-default and
experimental, with the remediation and remaining closure gates listed below. Legacy MCP SSE endpoints,
OAuth/browser authorization, resources, prompts, sampling, elicitation, automatic
server installation, and ambient discovery are unsupported.

## Security and authority

Installing or discovering this package is inert; the bridge is disabled by
default. Default full access (`unsafe_host`) implicitly trusts the selected
extension without persisting a grant, but startup still requires explicit
enablement, source/integrity validation, and the process gate. It starts no MCP
server unless an explicit configuration file exists; implicit extension trust
does not approve servers or tool calls.

A local MCP server is arbitrary software running with the current user's OS
authority. Neither the bridge, its manifest, nor per-call approval is an OS
sandbox. Review and install every server separately; this bundle never copies,
downloads, or installs server software.

There are two separate decisions:

1. **Server/endpoint trust:** the user configuration, or a user-file digest pin
   for a trusted project configuration, names either exact direct launch
   arguments or one exact remote endpoint.
2. **Tool-call policy:** server trust does not approve every tool. Only an MCP
   annotation whose `readOnlyHint` value is exactly JSON `true` (and which is
   not contradicted by positive destructive/open-world hints) receives the
   read-only classification. Missing, false, numeric, string, or malformed
   annotations are `unknown`. Destructive/open-world hints increase caution.

An explicitly read-only tool may run without an additional prompt. Every
`unknown` or `destructive` call goes through the negotiated host
`policy/evaluate` service. If policy intents are unavailable, evaluation fails,
or the host denies the intent, the bridge fails closed. It uses a one-use
approval retry only when the host actually negotiates `approvals`; octet `0.7.6`'s
coding product does not currently enable approval issuance, so those calls are
denied with an explanatory tool error. An MCP tool call is never automatically
replayed after timeout, cancellation, crash, or an ambiguous disconnect.

Server descriptions, schemas, logs, errors, and results are untrusted data.
Descriptions and schema text are bounded and explicitly labeled untrusted;
they cannot select lifecycle actions or lower policy. Compact presentation
never contains the server command, arguments, environment, credentials, raw
server descriptions, or raw logs. MCP stderr is drained into a bounded,
credential-redacted in-memory ring and is not copied into frontend state.

The server environment begins from a small non-secret process allowlist
(`PATH`, locale, and temporary-directory variables) plus only the `env` entries
in the explicit stdio configuration. It does not inherit dotenv files or ambient
provider/application tokens. Explicit `env` values are sensitive configuration:
protect the file and never place secrets in labels or arguments.

### Experimental Streamable HTTP gate

Local stdio MCP remains available through normal reviewed configuration. Remote
Streamable HTTP MCP is denied unless the **octet process owner** supplies this
one-shot command-line switch for that process:

```console
octet --experimental-streamable-http-mcp \
    --enable-extension octet-mcp
```

This is intentionally not a configuration feature. `~/.octet/mcp.json`,
digest-pinned project MCP files, octet project/global configuration, environment
variables, session/host requests, and extension-manifest arguments cannot grant
it. The coding product strips a manifest-supplied copy and passes the switch to
the bridge only after parsing the product CLI. A disabled remote template stays
inert; enabling it without the switch fails during configuration loading, before
credential lookup, DNS, network I/O, or MCP manager workers. Lifecycle actions
also reject a disabled-gate remote before creating a worker.

For direct development/configuration validation, the same owner switch is
required on the bridge command:

```console
octet-mcp --experimental-streamable-http-mcp --config ~/.octet/mcp.json --check-config
```

A remote endpoint is a separate explicit network-trust decision. Streamable HTTP
uses the exact configured URL, TLS certificate/hostname validation, no proxy or
cookie discovery, and no redirects. HTTPS is required except for a numeric
loopback address, which exists for deterministic local development and tests.
URLs cannot contain userinfo, a query, or a fragment, preventing URL-auth and
query credential fields as well as endpoint switching by redirect. The extension
never synthesizes a browser `Origin` header or forwards browser credentials.
DNS runs in an isolated, cancellable Python helper which is killed and reaped on
cancellation/shutdown. Every answer must be globally routable; mixed public/private
answers and mapped/transition addresses fail closed. Only an explicitly configured
literal loopback address is exempt. The reviewed numeric address is pinned for the
client lifetime, with no second DNS lookup during connection; TLS still uses the
original endpoint hostname for SNI and certificate verification. Non-public HTTPS
endpoints are not supported (apart from that literal loopback exception).

Remote `auth` never contains a token or header value; it names one of two
explicit sources. `{"type": "bearer", "credential": "<reference>"}` is a bounded
logical reference that only an explicitly composed
`CredentialProvider.bearer_token(reference, server_id=..., resource_owner=...)`
adapter can resolve; the bridge asks it at request time, uses the returned token
only to form that request's `Authorization: Bearer` header, redacts it from
parsed remote data, then drops it. The owner is an immutable host-issued
`ResourceOwner`, never a tool argument. Adapters must bind their lookup to that
complete owner and return promptly; a blocking application callback cannot be
forcibly killed inside Python. Its late return cannot initiate DNS or a
connection after cancellation/deadline.

`{"type": "static-bearer", "environment": "OCTET_MCP_<NAME>"}` is the only
bundled static source. It must name exactly one environment variable inside this
extension's own `OCTET_MCP_*` namespace; resolution is bound to that exact server
ID and configured static variable. A different server's `bearer` broker reference
never gains an environment fallback merely because a static server is present.
The bridge reads that name from the
process environment per request, so a rotated value is observed without
retaining it, and it refuses any other name so a configuration cannot point the
bridge at an unrelated ambient provider/cloud token. The value is never stored,
logged, echoed in an error or diagnostic, sent to `presentation`, or included in
result metadata; an unset or unnamespaced name fails closed as
`authentication_unavailable` before any socket is opened. Either way, an
owner-bound server whose source resolves nothing parks with
`authentication_unavailable`.

OAuth discovery, dynamic client registration, browser redirects, token
acquisition/refresh, keychains, dotenv files, persistent token stores, arbitrary
static config headers, and env-var fallback outside `OCTET_MCP_*` are **not**
implemented and are policy-gated. The exact missing primitive is a
host-brokered OAuth/credential authorization service negotiated over the
extension API (a typed `authorization/request` capability plus a host-owned
token store); nothing in the extension API or this package may substitute a
self-composed browser flow, and a configuration file must never be able to
widen it.

### Known Streamable HTTP defects

The gate remains a containment measure, not a production safety qualification.
Do **not** enable it for production, privileged networks, or sensitive credentials.
The original nine defects now have the following local remediation and regressions
in `tests/test_http_hardening.py`:

| Original defect | Current safeguard |
| --- | --- |
| 1. HTTPS DNS rebinding/SSRF | Validate all resolved addresses; reject non-public/special-use answers; connect only a pinned numeric address with original-host TLS verification. |
| 2. Cross-owner credentials and sessions | Require an immutable host owner before remote startup; pass it to credential composition; reject absent/foreign session, instance or generation before policy, credentials or I/O. No in-place owner migration. |
| 3. DNS outlives cancellation/shutdown | A bounded-output isolated helper is killable and reaped; sockets and helpers belong to tracked operations. |
| 4. Buffered SSE confuses peer identity | Route server requests/progress while reading; distinguish requests from terminal responses even when IDs collide; parse before payload redaction and reject foreign response IDs. |
| 5. Unbounded control fanout | At most 16 peer-request/catalog-change actions per operation and 16 concurrent reply/cancel workers per client; no unbounded control queue; tracked cancellation and absolute control watchdogs. |
| 6. Budgets reset across transport paths | One cumulative byte/event/control budget for a POST and all GET resumptions. |
| 7. Truncated framing accepted | Require complete SSE blank-line boundaries, exact Content-Length and strict chunk separators/final trailer terminator; ambiguous framing fails closed. |
| 8. Empty SSE ID retains stale cursor | Commit IDs at complete event boundaries; empty ID clears the operation-local cursor and prevents another GET; absent ID preserves it. |
| 9. Startup deadline renewed | One absolute deadline covers initialize, initialized notification and every initial catalog page, including admission, DNS and resumptions. Refresh has one deadline across all pages. |

**Remaining closure gates:** this is still a candidate, not general availability.
Local TLS/HTTP adversarial fixtures do not qualify external-server interoperability,
Linux/platform cleanup, long-duration resource pressure or host/Serve owner changes.
A resident binds to only one host owner: remotes initially park with
`resource_owner_required`; run `/mcp restart <server>` from an owned command to
connect. A different owner requires restarting the extension process. Automatic
multi-owner partitioning, owner-settlement cleanup and host-qualified owner-specific
catalog visibility remain unimplemented; foreign tool calls fail closed. These
limitations must not be mistaken for a fully shared remote service.

Static, extension-scoped credentials and the optional permanent GET notification
stream are now implemented and covered by deterministic loopback regressions
(`tests/test_streamable_http.py`), so the previous "no static credentials / no
permanent GET stream" defects are closed locally. Two gates remain open:

- **OAuth/credential brokering is policy-gated, not missing by accident.** The
  exact missing primitive is a host-brokered authorization service negotiated
  over the extension API — a typed `authorization/request` capability with a
  host-owned token store and refresh ownership. Nothing in the bundled API `0.2`
  surface can express it, and this package will not substitute a self-composed
  browser/OAuth flow or read an ambient provider token.
- **No live remote qualification.** All stream/credential evidence comes from
  deterministic loopback HTTP fixtures; no external MCP server, real credential,
  real OAuth server, or long-duration stream has been exercised here.

An injected synchronous credential/progress callback is trusted application code;
Python cannot forcibly terminate it. Its operation remains bounded in admission
and late network activity is fenced, but full cleanup cannot be guaranteed until
it returns. An unresolved `bearer` reference still has no stock credential
adapter. API `0.2` product integration and release qualification remain
independent gates.

## Requirements and installation

- octet exactly `0.7.6` (`requires_octet = "=0.7.6"`)
- Python 3.9 or newer on `PATH`
- separately installed MCP server executables

The release bundle includes the dependency-free Python extension SDK under
`vendor/`; startup never runs `pip`, a browser download, or install code.

With [octet 0.7.6 installed](../../docs/installation.md), install the matching
signed public bundle, then explicitly enable it:

```console
octet extension install octet-mcp
octet --enable-extension octet-mcp
```

For local development, select a reviewed checkout explicitly without installation:

```console
octet --extension-dir ./extensions \
    --enable-extension octet-mcp
```

`--trust-extension` and source-bound `trusted_extensions` grants remain optional
explicit trust decisions, never enablement. Full-access implicit trust is not
saved as a grant. `--safe-mode` removes implicit trust and retains discovery but
never starts this bridge, even with explicit grants: executable startup still
requires `unsafe_host`. Neither mode supplies an OS sandbox.

## Configuration

The normal entrypoint reads `~/.octet/mcp.json`. If that file is absent, the
bridge remains healthy with zero configured servers. Copy the disabled example
and edit it deliberately:

```console
mkdir -p ~/.octet
cp ~/.octet/extensions/octet-mcp/config.example.json ~/.octet/mcp.json
chmod 600 ~/.octet/mcp.json
$EDITOR ~/.octet/mcp.json
~/.octet/extensions/octet-mcp/octet-mcp --config ~/.octet/mcp.json --check-config
```

If the file enables a Streamable HTTP server, add
`--experimental-streamable-http-mcp` to that validation command and to the octet
process that will own the bridge. The JSON has no equivalent setting.

`config.schema.json` is the normative JSON schema. The parser also rejects
unknown/duplicate keys, non-UTF-8 or oversized files, symlink final files,
linked `.octet` roots or escaping trusted-project ancestors, files writable by
another user, files with explicit `env` values accessible by group/other users,
duplicate server IDs, NUL/control characters, mutually incompatible transport
fields, unsafe remote URLs, and values outside package ceilings. Commands are
direct argument arrays and never pass through a shell. Remote endpoints are exact
URL strings rather than discovery patterns; there is no raw `headers`, token,
password, or OAuth configuration field. The one static credential form names an
extension-scoped `OCTET_MCP_*` environment variable and never carries its value.

A minimal user file is:

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

Server IDs are stable lowercase identifiers matching
`[a-z][a-z0-9-]{0,31}`. Labels are trusted user text; the MCP server's own name
is never used as a UI label. Relative `cwd` values resolve from the file that
defines the server. A missing executable or invalid MCP handshake is a
permanent failure parked until an explicit restart/config refresh.

### Streamable HTTP configuration

This experimental configuration is inert without the process-owner
`--experimental-streamable-http-mcp` flag. Adding the flag to JSON, a trusted
project file, an environment variable, or a session does not work.

A remote server must opt into `"transport": "streamable-http"` and name one
absolute endpoint. It accepts `https`; `http` is accepted only for literal
`127.0.0.1` or `::1` loopback endpoints. Stdio launch fields (`command`, `args`,
`cwd`, and `env`) are rejected for this transport.

```json
{
  "version": 1,
  "servers": {
    "remote-example": {
      "transport": "streamable-http",
      "label": "Reviewed remote MCP",
      "url": "https://mcp.example.invalid/mcp",
      "enabled": true,
      "required": false,
      "auth": {
        "type": "bearer",
        "credential": "reviewed_remote_mcp"
      }
    }
  }
}
```

`credential` is a bounded logical reference, not a secret. It requires an
application-provided `CredentialProvider`; the stock executable fails closed
without one. The bundled static form is likewise explicit and extension-scoped:

```json
{
  "version": 1,
  "servers": {
    "remote-static-example": {
      "transport": "streamable-http",
      "url": "https://mcp.example.invalid/mcp",
      "auth": {"type": "static-bearer", "environment": "OCTET_MCP_REMOTE_TOKEN"}
    }
  }
}
```

`environment` must match `OCTET_MCP_` followed by uppercase letters, digits, or
underscores (at most 48 more bytes). The bridge reads exactly that variable from
the process environment on each request, holds no token, and never logs, echoes,
or publishes it; an unset variable fails closed as `authentication_unavailable`.
Any other name — including an ambient `OPENAI_API_KEY`-style provider token — is
rejected by the parser and by the bundled source. Omit `auth` for an endpoint
that does not need authorization. Do not put tokens in a URL, label, argument,
`env`, or `headers`: the parser still rejects a `headers` field and any
`auth` field that would carry a literal token, and this package deliberately
offers no arbitrary static header or OAuth configuration.

### Digest-pinned trusted project configuration

Project files are not discovered or launched on their own. The user file must
name an **absolute** file beneath the active workspace's `.octet/` directory and
pin its exact bytes:

```json
{
  "version": 1,
  "servers": {},
  "trustedProjects": [
    {
      "path": "/absolute/workspace/.octet/mcp.json",
      "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    }
  ]
}
```

Generate the digest after review (for example, `shasum -a 256 FILE`). Any edit
invalidates the pin and leaves the bridge in an inspectable degraded state; it
does not execute the changed project command. Project files may contain only
`version` and `servers`, cannot include another file, and cannot override a user
server ID.

### Enforced default bounds

| Resource | Default | Package maximum |
| --- | ---: | ---: |
| configured servers | 16 | 32 |
| tools per server | 64 | 128 |
| total published tools | 256 | 256 |
| catalog pages | 8 | 32 |
| MCP frame/result | 8 MiB | 16 MiB |
| concurrent calls | 8 | 32 |
| pending requests per server | 16 | 64 |
| retained stderr entries | 128 | 1,024 |
| retained stderr line | 4 KiB | 16 KiB |
| startup timeout | 5 s | 30 s |
| request timeout | 30 s | 120 s |
| shutdown stage | 1.5 s | 5 s |
| automatic restart attempts | 5 | 8 |
| retry backoff cap | 30 s | 60 s |

Catalog pagination is cycle-checked. Tool/schema text, structured output,
content parts, text, individual/aggregate media, presentation nodes,
activities, and action counts have additional fixed bounds in source.

### Streamable HTTP framing and recovery

This describes the experimental path, subject to the
[remaining closure gates](#known-streamable-http-defects), not a safety qualification.

The HTTP client POSTs one JSON-RPC message with `Content-Type: application/json`
and `Accept: application/json, text/event-stream`. It accepts a bounded JSON
response or a bounded SSE response; `202 Accepted` is accepted only for
notifications. After a valid `initialize` response, a validated in-memory
`Mcp-Session-Id` is sent on subsequent requests with the negotiated
`MCP-Protocol-Version`. A changed or malformed session identity fails closed.
A `404` for an established session is treated as expiration and triggers the
normal fresh-session reconnect path.

SSE response events are UTF-8 JSON-RPC `message` events, bounded by the existing
frame limit both per event and cumulatively across the POST and all resumed GETs
(at most 256 event blocks, including ignored/control blocks). Peer requests and
progress route as complete events arrive, not after the final response. A server-issued
SSE `id` is retained only in that operation's memory; an empty ID clears it. If a POST response stream closes before its
terminal response *after* such an ID, the bridge may perform at most the
configured `maxRestarts` bounded GET resumptions with `Last-Event-ID`; it never
re-POSTs the original request. Without an ID, an interrupted request is
ambiguous and is not replayed. Server-provided SSE `retry` values are capped by
the configured backoff maximum.

After the `initialize` response and `notifications/initialized`, the bridge opens
the optional permanent GET notification stream **only when the negotiated server
capabilities actually declare a `listChanged: true` capability**. That stream is
best-effort and never gates a request: `405 Method Not Allowed` marks it
`unsupported` in one bounded log line and it is never retried, while every POST
path keeps working. Each connection is renewed inside the configured
`requestTimeoutMs`, so an idle stream connection is a normal renewal rather than
a failure; a connection ends with a committed SSE event ID, the next connection
sends that exact `Last-Event-ID`, and a peer that replays the acknowledged
identity fails closed with `sse_event_replayed` (an empty `id:` clears the
cursor). Its committed cursor is memory-only, exactly like the session identity.
Failed connections use `backoffInitialMs`→`backoffMaxMs` and are bounded by
`maxRestarts` consecutive failures and 64 lifetime failures. Healthy completed
streams and renewals after an established SSE response do not consume either
failure budget. A timeout before establishing the SSE response counts as a
failure. Exceeding either bound ends the stream through the normal bounded
lifecycle failure path. The retired standalone/legacy SSE transport is still not
implemented.

HTTP response bodies, event streams, request slots, timeouts, and shutdown use
the configured bounds, subject to the closure gates above. Redirects are rejected
before following a `Location`; 401/403 park with a generic authentication error;
malformed, oversized, unsupported-content-type, and unsafe status responses park
without retaining response text. Rate limits and transient transport/server
failures use the existing bounded lifecycle backoff (honouring a capped numeric
`Retry-After` when present). Cancellation aborts the in-flight socket and sends
at most one best-effort bounded `notifications/cancelled`; it never claims rollback or replays the
request.

## Catalogs, calls, and results

The initialize catalog is empty epoch `0`; configured servers start only after
the octet initialize response has been flushed. Each successful server catalog
change publishes complete dynamic definitions. The bundled API `0.2` SDK keeps
eight committed octet schema/handler snapshots, so an older in-flight model turn
uses the handler and validation schema from the `catalog_revision` it saw.
Removed/restarted servers never alias an old epoch to a new connection.

The bridge filters unsupported schema keywords at every schema node, including
schema-valued `additionalProperties`. Recursive MCP `$defs`/`$ref` constraints
cannot be enforced by octet's tool bus; omitting them may make a catch-all
schema permissive, not grant approval for a tool call. The read-only annotation
and host policy gates remain separate.

Calls use bounded concurrency and timeout, forward octet cancellation as MCP
`notifications/cancelled`, and retain safe server/tool provenance and terminal
activity. Cancellation requests cooperation and never claims rollback.
Server-reported progress is reduced to bounded numeric progress; untrusted
progress messages are not promoted to UI authority.

MCP results cross the normal API `0.2` boundary:

- text remains ordered model-visible text;
- an MCP `structuredContent` paired with `outputSchema` becomes validated octet
  `structured_content`;
- schema-less structured content is retained in bounded, non-model-visible
  metadata because API `0.2` forbids `structured_content` without a declaration;
- supported image/audio base64 is written to the generation scratch directory,
  published through `artifact/publish`, then removed locally; and
- malformed, unsupported, or oversized content returns a bounded tool error.

Supported media matches the host artifact verifier: PNG, JPEG, GIF, WebP, WAV,
MPEG audio, FLAC, Opus, AAC, and MP4 audio. Artifact IDs remain bound to the
active host-derived session owner and process generation.

## Lifecycle, health, and recovery

Servers transition through configured, connecting, ready, refreshing,
degraded, backoff, parked, and stopped states. Transient crashes reconnect with
bounded full-jitter exponential backoff. Permanent configuration/protocol
failures and exhausted retry budgets park. A successful connection and catalog
publish resets the restart failure counter, so healthy sessions do not deplete
it. Refresh reads a catalog without
relaunching; restart explicitly replaces a connection; stop removes its current
tools and closes it. Shutdown closes all roots in bounded parallel workers, and
octet's extension process-group cleanup is the final descendant fence.

Use the narrow/headless fallback in every frontend:

```text
/mcp status
/mcp list
/mcp snapshot
/mcp show <server>
/mcp refresh [server]
/mcp restart <server>
/mcp stop <server>
```

`/mcp snapshot` returns the same generic semantic snapshot published through API
`0.2` `presentation/update`. Lifecycle actions route only to the manifest-
declared `mcp` command with literal bridge-authored arguments; server text and
model output cannot manufacture an action.

## TUI and Serve presentation

The package emits only octet's generic semantic presentation contract—compact
status, bounded activity, a host-rendered server/tool tree, selected detail, and
declared actions. It ships no ANSI renderer, terminal widget, web JavaScript, or
MCP manager in core. TUI and Serve remain projections of the resident bridge.

The compact status is like `mcp 2/3 · 7 tools · degraded`. Server nodes expose
safe lifecycle/transport/catalog/restart metadata; tool nodes expose only their
sanitized octet name, schema counts, and approval classification. Complete
snapshots make reconnect/resync side-effect-free: reading state never launches a
server, refreshes a catalog, repeats a call, or revives a retired epoch. The host
process generation fences stale removal after bridge reload.

`fixtures/presentation/` contains deterministic generic snapshots for empty,
connecting/loading, ready, refreshing, degraded, parked, restarted, running,
succeeded, failed, cancelled, and ambiguous states, plus official Serve
projection fixtures for reconnect/resync and stale-generation removal. The same
fixtures are frontend-neutral and are intended for both TUI and Serve reducers.

## Tests

From the package root (the release-manifest test requires Python 3.11+
for stdlib `tomllib`; runtime transport tests also run on Python 3.9):

```console
python3 -m unittest discover -s tests -t . -v
```

The dependency-free suite covers strict config/trust, real and adversarial stdio
servers, deterministic loopback Streamable HTTP framing/session/auth/SSE fixtures,
add/replace/remove catalogs, epoch-pinned schemas, malformed/oversized frames,
cancellation, timeout, crash/restart/parking, bounded redacted logs, media
artifacts, policy failure, shutdown, API `0.2` wire behavior, generic
presentation fixtures, and release/package smoke checks. This inventory is not
live remote-transport or release qualification. The local TLS fixture uses a
[deliberately public test-only key](fixtures/tls/README.md), never a real credential.
