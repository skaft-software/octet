# octet-mcp reference

[Usage guide](README.md). This is the bundled API `0.2` implementation contract,
not a current extension-authoring example. Distribution `0.7.3` requires exactly
octet `0.7.3`; these version numbers are independent.

One resident extension process owns every explicitly configured
[Model Context Protocol](https://modelcontextprotocol.io/) server session and
maps its live tool catalog to transactional `tools/register` and
`tools/unregister` calls.

```text
octet <- API 0.2 JSON-RPC -> octet-mcp <- MCP JSON-RPC stdio -> local servers
                                 \-> MCP Streamable HTTP -> explicit remote endpoint
```

Local stdio is the normal transport. This is an **unreleased source candidate**;
new behavior requires the matching source host and bundle, not a published
`0.7.3` installation. Streamable HTTP remains blocked-by-default experimental.
Tools, resources/templates/reads, explicit modern HTTP and private authentication
have bounded implementations, not production qualification. Legacy standalone SSE,
prompts, sampling, roots, automatic installation and ambient discovery remain
unsupported. [QUALIFICATION](QUALIFICATION.md) retains the full compatibility scope,
including remaining modern subscription/authorization/interaction gaps.

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
approval retry only when the host actually negotiates `approvals`. The matching
source coding host installs a generic exact-call adapter only for isolated API
`0.2` `octet-mcp`. It can ask, never blanket-allow: complete host-authored tool,
argument, catalog and owner/generation/parent evidence is bound into the short-lived
one-use approval. Missing/noninteractive UI, stale/changed/cancelled calls and
oversized or incompletely reviewable previews deny. The bridge rechecks captured
catalog/connection authority at wire dispatch. Other extensions retain the default
deny policy. This is not #383's typed app/origin/observation automation adapter;
server annotations remain untrusted classifications, not automation safety proof.
An MCP tool call is never automatically replayed after timeout, cancellation,
crash, or an ambiguous disconnect.

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

Remote `auth` contains only logical references and reviewed public OAuth
configuration, never a token or static header. The stock source runtime composes
an owner-aware private credential provider with the existing bridge. A connection
captures its exact host owner; discovery, reconnect, cancellation and background
catalog reads cannot borrow another owner or the last caller's credentials.
Tokens are resolved at request time, redacted from parsed remote data and omitted
from status, presentation and tool diagnostics. See [private authentication](#private-authentication).

### Known Streamable HTTP defects

The gate is containment, not a production-safety claim. Do **not** enable remote
MCP for production, privileged networks, or sensitive credentials. All nine
original defect IDs remain in [the qualification ledger](qualification/baseline.json):

| ID | Candidate remediation / regression boundary |
| --- | --- |
| `D-HTTP-01` | Validate every DNS answer and connect to a checked numeric address; retain original TLS SNI/hostname and HTTP Host. Local real-TLS tests also reject wrong names/untrusted certificates. |
| `D-HTTP-02` | One exact host owner/session per isolated bridge generation; per-connection credentials, sessions, callbacks, cancellation, historical handlers and presentation are fenced. |
| `D-HTTP-03` | DNS runs in a killable, bounded subprocess, terminated and reaped on cancellation/shutdown. |
| `D-HTTP-04` | SSE peer requests cannot masquerade as client responses; preserve exact peer identity while redacting data. |
| `D-HTTP-05` | Four concurrent control operations, sixteen control messages per originating operation, bounded deadlines. |
| `D-HTTP-06` | POST/resumption share aggregate byte/event/control budgets rather than resetting them. |
| `D-HTTP-07` | Reject truncated lengths, chunks/trailers and unterminated SSE events, even after a terminal-looking payload. |
| `D-HTTP-08` | Empty SSE IDs clear the cursor and forbid further stale-cursor resumptions. |
| `D-HTTP-09` | One startup deadline covers initialization, initialized notification and initial catalog pagination/cleanup. |

Implementation and controlled regressions are not proof of live DNS/TLS,
endpoint, authorization, frontend or platform qualification. The source matrix
assesses the pinned input; candidate evidence is recorded separately. Keep the
flag and #179 open until the applicable release gates and supported real-world
journeys are qualified. None of the framing details below waive that boundary.

## Requirements and installation

- octet exactly `0.7.3` (`requires_octet = "=0.7.3"`)
- Python 3.9 or newer on `PATH`
- separately installed MCP server executables

The release bundle includes the dependency-free Python extension SDK under
`vendor/`; startup never runs `pip`, a browser download, or install code.

For this candidate, build the matching source host and select the reviewed
checkout as below. These managed-install commands apply only after a matching
host/bundle release, not to the already published `0.7.3` implementation:

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
URL strings rather than discovery patterns; there is no raw `headers`, token or
password field. OAuth configuration holds only a reviewed issuer, public client
ID, optional scope ceiling and loopback redirect port.

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

`credential` is a bounded logical reference, not a secret. The source runtime's
`/mcp auth login <server>` supplies its owner-private bearer value. Omit `auth`
for an endpoint that does not need authorization. Never put tokens in a URL,
label, argument, `env` or any other remote config field. Static HTTP headers are
not supported. All hostname DNS answers must be globally routable; prohibited
private/link-local/reserved destinations fail before connection. Explicit numeric
loopback is only for reviewed local development.

### Private authentication

OAuth currently requires a reviewed **pre-registered public client** and exact
issuer; it does not impersonate Codex's client identity. Replace the `auth`
object with:

```json
{
  "type": "oauth",
  "credential": "reviewed_remote",
  "issuer": "https://issuer.example.invalid",
  "clientId": "YOUR_REVIEWED_PUBLIC_CLIENT_ID",
  "scopes": ["reviewed-scope"],
  "redirectPort": 0
}
```

An omitted scope ceiling differs from explicit `[]`, which permits no additional
scope. The client checks protected-resource and OAuth/OIDC metadata, exact
resource/issuer/token-endpoint binding, public-client authentication, S256 PKCE,
state, callback issuer and scope responses. Networking has bounded admission,
DNS/TLS pinning, deadlines, no redirects/proxies/cookies and no ambient credentials.

- `/mcp auth login <server>` requests bearer input privately or begins manual OAuth.
  A bounded numeric-loopback callback uses a random path and port (or the reviewed
  `redirectPort`). After secret-free initial consent, review the authorization URL
  in private input and type `continue`. Open it manually and finish login after
  the command returns. No browser is launched automatically; the URL/state never
  enters ordinary confirmation details.
- `/mcp auth poll <server>` finishes a received callback and token exchange.
- `/mcp auth login-manual <server>` and `complete-manual` use private callback-URL
  input for a separate browser; never paste a callback URL into chat or arguments.
- `status` is observational; `cancel` retires a pending login; `logout` removes
  local credentials and stops the bound MCP connection, without remote revocation.
- After successful setup, explicitly `/mcp restart <server>`. No login, refresh or
  transport error causes a tool replay. Credential replacement stops the old
  connection first so an authenticated MCP session cannot silently change identity.

The POSIX store is `~/.octet/mcp-auth`: no-follow path checks, private `0700`
directories, regular single-link `0600` files, locked/atomic updates. It is
**plaintext**, not an encrypted vault or protection against the same OS principal,
root or backups. Deletion is not secure erasure. Persistent keys include durable
host owner, server, exact endpoint/configuration, issuer/client and scopes;
active flows additionally bind extension instance/generation. A fresh generation
of the same durable owner can reuse credentials; other owners cannot.

Refresh is serialized, expiry-aware and fail-closed on scope expansion, changed
metadata, non-rotating refresh tokens or ambiguous exchanges. Local removal does
not revoke a remote authorization grant; use the issuer's manual revocation UI.
Dynamic registration, an Octet-published CIMD identity, enterprise authorization,
client secrets, sender-constrained tokens and automatic scope escalation remain
unimplemented. Transport `WWW-Authenticate` challenges are not yet carried into
explicit login; configured issuer and standard well-known discovery are required.
If every remote is disabled, explicitly restart the intended server to bind its
owner before setup. A passing fake issuer is not live OAuth qualification.

### Modern HTTP mode

Only an explicitly configured remote `"protocolVersion": "2026-07-28"` selects
this mode. Omission retains legacy initialization; no automatic negotiation
fallback or failed-tool replay occurs. Modern mode uses `server/discover`,
self-contained protocol/client capability metadata and matching `MCP-Protocol-Version`,
`Mcp-Method`, applicable `Mcp-Name` and schema-derived `Mcp-Param-*` headers.
`x-mcp-header` annotations are validated for placement, primitive types and
case-insensitive collisions, and pinned to the tool's accepted epoch.

Modern HTTP sends no initialize/initialized, legacy session header, GET resume,
Last-Event-ID or DELETE. Cancellation closes its response stream. Modern errors
and non-complete results are not interpreted as successful empty content. Tools
and resource operations have explicit method admission; subscriptions/listen,
automatic version fallback, and modern stdio are not implemented. Standard private
interaction/MRTR bounds are described below; unsupported capabilities are not
advertised merely because the protocol schema defines them.

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

This describes the experimental path, subject to all
[known defects](#known-streamable-http-defects), not a safety qualification.

The HTTP client POSTs one JSON-RPC message with `Content-Type: application/json`
and `Accept: application/json, text/event-stream`. It accepts a bounded JSON
response or a bounded SSE response; `202 Accepted` is accepted only for
notifications. After a valid `initialize` response, a validated in-memory
`Mcp-Session-Id` is sent on subsequent requests with the negotiated
`MCP-Protocol-Version`. A changed or malformed session identity fails closed.
A `404` for an established session is treated as expiration and triggers the
normal fresh-session reconnect path.

SSE response events are UTF-8 JSON-RPC `message` events, bounded by the existing
frame limit both per event and in aggregate (at most 256 events). A server-issued
SSE `id` is retained only in memory. If a POST response stream closes before its
terminal response *after* such an ID, the bridge may perform at most the
configured `maxRestarts` bounded GET resumptions with `Last-Event-ID`; it never
re-POSTs the original request. Without an ID, an interrupted request is
ambiguous and is not replayed. Server-provided SSE `retry` values are capped by
the configured backoff maximum. This path does not open a permanent optional GET
notification stream and does not implement the retired standalone/legacy SSE
transport.

HTTP response bodies, event streams, request slots, timeouts, and shutdown use
the configured bounds, subject to the defects above. Redirects are rejected
before following a `Location`; 401/403 park with a generic authentication error;
malformed, oversized, unsupported-content-type, and unsafe status responses park
without retaining response text. Rate limits and transient transport/server
failures use the existing bounded lifecycle backoff (honouring a capped numeric
`Retry-After` when present). Cancellation aborts the in-flight socket and sends
one bounded `notifications/cancelled`; it never claims rollback or replays the
request.

## Catalogs, calls, and results

The initialize catalog is empty epoch `0`; configured servers start only after
the octet initialize response has been flushed. Each successful server catalog
change publishes complete dynamic definitions. The bundled API `0.2` SDK keeps
eight committed octet schema/handler snapshots, so an older in-flight model turn
uses the handler and validation schema from the `catalog_revision` it saw.
Removed/restarted servers never alias an old epoch to a new connection.

Calls use bounded concurrency and timeout, forward octet cancellation as MCP
`notifications/cancelled`, and retain safe server/tool provenance and terminal
activity. Cancellation requests cooperation and never claims rollback.
Server-reported progress is reduced to bounded numeric progress; untrusted
progress messages are not promoted to UI authority.

MCP schemas are bounded and must preserve their accepted semantics. Local acyclic
`$ref`/`$defs` are resolved with shared depth/node/expansion budgets; supported
composition, enum/const, type and numeric/size constraints are validated. External,
recursive/dynamic references, unsupported dialects/keywords (including `pattern`
and `format`) and malformed schemas are rejected, never silently dropped into an
unconstrained tool. This remains a supported subset, not unrestricted JSON Schema.

MCP results cross the normal API `0.2` boundary:

- text remains ordered model-visible text;
- an MCP `structuredContent` paired with `outputSchema` becomes validated octet
  `structured_content`;
- schema-less structured content is retained in bounded metadata because API
  `0.2` forbids an undeclared `structured_content` field. When there is no nonempty
  text, the extension explicitly renders bounded untrusted JSON as text so the
  model receives the data; this is not a change to host metadata visibility;
- resource links and embedded resource content are preserved as bounded untrusted
  data, without dereferencing their URIs;
- supported image/audio base64 is written to the generation scratch directory,
  published through `artifact/publish`, then removed locally; and
- malformed, unsupported or oversized content returns a bounded tool error,
  never silent truncation or an empty success for `input_required`.

Supported media matches the host artifact verifier: PNG, JPEG, GIF, WebP, WAV,
MPEG audio, FLAC, Opus, AAC, and MP4 audio. Artifact IDs remain bound to the
active host-derived session owner and process generation.

## Resources and private interactions

A server advertising `resources` receives three collision-safe bridge-authored
catalog entries: `mcp_resources_<server>_list`, `_templates` and `_read` (hyphens
in the configured server ID become underscores). They invoke resource protocol
methods on the captured connection, never upstream `tools/call`. Listing is fresh,
fully paginated under one deadline, bounded to 128 entries with cycle checks.
Reads take one explicit opaque `uri`, retain bounded text or base64 binary JSON,
and share ordinary call admission, owner/cancellation and stale-connection fences.
They never open host files, fetch a URL, follow a link implicitly or cache across
owners. Resource-change subscriptions are not implemented; refresh/relist is explicit.

Private HTTP elicitation is opt-in at the client boundary and tied to an admitted
operation. Only bounded flat primitive/enum form schemas are supported; unsupported
or credential-like forms fail closed. URL mode presents a reviewed HTTPS URL in
private UI for manual action, never automatically opens/fetches it. Unknown,
unsolicited, stale, cancelled or unavailable private requests deny. Input/consent
is not blanket action authority and does not perform OAuth login automatically.

The matching TUI presents complete escaped private context ephemerally, not in
transcript history. Raw and escaped prompts are bounded to 16 KiB, answers to
4096 bytes; the entire context must fit the actual viewport without scrolling.
Clipping, overflow, unsuitable resize and configured `OCTET_TUI_WRITE_LOG` deny
rather than accept a blind answer. Secret answers remain hidden. Serve has no
private prompt channel: tool input cancels without a public pending request, and
headless commands deny setup. This is an explicit frontend limitation, not
permission to send credentials through chat or ordinary public confirmations.
Small-form/URL renderer fixtures are not combined live MCP/OAuth qualification.

Modern `input_required` continuation is confined to `tools/call` and
`resources/read`: original arguments remain fixed, each continuation gets a fresh
request ID and exact operation-local opaque `requestState`, under one absolute
deadline and aggregate budget. Limits include four continuations, four input
requests per round, eight private interactions, 64 KiB state and 32 MiB aggregate
operation data. Errors/loss are not retried. Unsupported result types and private
requests cannot become successful empty content. Credential redaction remains
active for catalogs and unrelated data; only the trusted, matched modern
operation preserves its exact private continuation fields. A server-supplied
`resultType` cannot disable generic redaction. Legacy stdio elicitation and
modern subscription streams remain explicit qualification gaps.

## Lifecycle, health, and recovery

Each isolated resident process admits at most one complete remote owner triple
and its separately correlated host lifecycle session. Remote startup waits for
`before_prompt` or an explicit action with that owner; observations alone never
start servers. Missing, foreign, stale or settled owners fail closed. Settlement
revokes the scope before queued handlers, removes catalogs, closes sockets and
cancels reconnect/auth work. Another session needs host replacement/reload, not a
mutable default owner. Local stdio retains its existing process lifetime.

Servers transition through configured, connecting, ready, refreshing,
degraded, backoff, parked, and stopped states. Transient crashes reconnect with
bounded full-jitter exponential backoff. Permanent configuration/protocol
failures and exhausted retry budgets park. Refresh reads a catalog without
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

From the package root (Python 3.11+ for the release manifest test's standard-library
`tomllib`; the bridge runtime itself supports Python 3.9+):

```console
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -t . -v
python3 qualification/check.py --verify-local-snapshot
```

`qualification/check.py --require-qualified` intentionally fails while the ledger
retains open gates. A source hash check, fixture success or protocol version
constant does not establish runtime, frontend, live-provider or Codex parity.

The dependency-free suite covers strict config/trust, real and adversarial stdio
servers, deterministic loopback Streamable HTTP framing/session/auth/SSE fixtures,
add/replace/remove catalogs, epoch-pinned schemas, malformed/oversized frames,
cancellation, timeout, crash/restart/parking, bounded redacted logs, media
artifacts, policy failure, shutdown, API `0.2` wire behavior, generic
presentation fixtures, and release/package smoke checks. This inventory is not
live remote-transport or release qualification.
