# Pi session/transcript import — bounded design and missing artifact

**Issue:** [#4](https://github.com/skaft-software/octet/issues/4)
**Status:** NOT IMPLEMENTED. The Pi session/transcript on-disk format is **not
establishable from this tree**, so no parser was written. Guessing a format would
mean silently mis-importing other people's conversation history, so the exact
missing artifact is recorded here instead, together with the bounded design and
acceptance criteria the eventual implementation must satisfy.

`octet migrate import pi` today imports settings, model selection, skill content,
and local stdio MCP declarations. It does not read, convert, or write Pi sessions
or transcripts.

## Search inventory (what was checked, and what was found)

| Checked | Finding |
| --- | --- |
| `crates/octet-coding-agent/src/migrate.rs` | Imports settings/models/skills/MCP only. `:2406` records Pi's `eventBus` as a *surface* during static analysis; nothing reads a session or transcript file. `MigratedSetup` has no session/transcript field. |
| `protocol/extension-api-v0.3.schema.json` `MigrationImportResult` | Bounded `models`, `skills`, `mcp_servers`, `diagnostics` (`sdk/python/octet_extension/api_v03.py`). No session/transcript variant exists in the current API. |
| `extensions/octet-import-pi/README.md` | The adapter deliberately re-execs `octet migrate adapter pi`; it owns no parsing and documents no session format. |
| `extensions/octet-pi-compat/README.md:148` | "session entries … remain explicit blockers" for parity. |
| `extensions/octet-pi-compat/bridge.mjs:903` | `makeThrowingProxy("ctx.sessionManager")` — the bridge rejects the session-manager surface instead of modelling it. |
| `extensions/octet-pi-compat/profiles/0.84.4.json:128`, `COMPATIBILITY.md:175` | `sessionManager` is a known-but-rejected context surface ("host-owned … rejected explicitly"). |
| `extensions/octet-pi-compat/tests/fixtures/fake-pi/dist/index.js:648` | The fixture returns `sessionManager.getEntries()` for the recorded probe only; it is not an on-disk format and has no entry schema. |
| `docs/pi-migration.md:224` | "session/tree/compaction or agent-control mutation" is explicitly outside the supported boundary. |
| Repository-wide search for a Pi session store | No Pi session/transcript reader, writer, fixture, schema, or golden capture exists anywhere in the tree. Every `jsonl` mention in the tree — octet telemetry, Harbor evaluation sessions, packaging tests, `extensions/octet-serve/src/usage.rs` — is an octet/evaluation format, never a Pi session store. |

Conclusion: the *extension-facing* Pi session-manager surface is known and
rejected, but the *durable* session/transcript representation (container, entry
discriminants, versioning, cursor/parent links, compaction markers) is absent
from the tree. There is nothing to validate a parser against.

## Exact missing artifact

One of the following, pinned to the compatibility reference Pi
(`8a7b0c03dfb702663acafb6dc29f8acaa4ffe391`, package `0.84.4`):

1. **An upstream format specification** for the Pi session store (container type,
   file naming, version field, entry discriminants, required/optional fields,
   id/parent/cursor semantics, compaction and tree markers, tombstone/abort
   records, encoding and ordering rules); **or**
2. **A reviewed, redacted capture corpus**: at least three real Pi session
   directories produced by the pinned package, covering a fresh session, a
   resumed multi-turn session with tool calls, and a session with compaction and
   a branch/fork, plus the exact Pi version and package integrity digest that
   wrote them. Captures must be owner-redacted (no prompts, credentials, or
   private paths) and committed as bounded fixtures, never as live homes.

Until one of these exists, the correct behavior is to report the artifact as
unavailable and import nothing. `octet migrate import pi` must keep saying so,
with exit code and diagnostic, rather than reporting an empty successful import.

## Bounded design for the eventual import

Untrusted-input rules (a transcript is attacker-influenced data, even from the
user's own machine: any process or download can write it):

- **Bounded before parsing.** Reject the whole import when the store exceeds any
  bound: total bytes, file count, per-file bytes, entries per session, field
  bytes, string bytes, JSON/frame nesting depth, and line length. Defaults must
  mirror existing migration bounds (`MigrationImportResult` limits in
  `protocol/extension-api-v0.3.schema.json`); at minimum a session import needs
  an entry-count cap, a total-byte cap, and a per-string cap, all configurable
  downward only.
- **Shape-validated, schema-versioned.** Every entry is validated against an
  explicit discriminated shape; unknown entry types, unknown fields, missing
  required fields, out-of-order or duplicate ids, dangling parent/cursor links,
  and non-monotonic timestamps fail closed with a bounded diagnostic naming the
  entry index — never a partial silent conversion.
- **Never execute anything derived from it.** No tool call is replayed, no shell
  command is run, no provider request is made, no MCP server is started, no
  extension is loaded, no skill/instruction file from the transcript is applied
  or trusted. Imported content is inert session *data*.
- **Never widen trust.** Import must not change persisted project trust, must not
  grant capabilities or approvals, must not inherit Pi credentials or
  environment, and must not modify the source store. Imported sessions land
  inactive: no automatic run, resume, or continuation.
- **Redact on the way in.** Credential-shaped values, absolute/home paths, and
  known secret shapes are dropped with a counting diagnostic (same fail-closed
  posture as the bus/DTO validators). Transcript text that looks like a secret
  must never reach a session file, telemetry, or the model prompt.
- **Idempotent and reversible.** Re-import of the same session converges to the
  same result; existing sessions are never overwritten without an explicit
  conflict decision; the operation is dry-run first with a bounded report.

Acceptance criteria for the implementation:

1. A documented format source (spec or corpus) is committed with integrity data.
2. Fixture tests cover: well-formed import; oversized store; unknown field;
   unknown entry type; dangling parent; duplicate id; non-monotonic cursor;
   embedded credential/PII-shaped value; source-store immutability; destination
   trust/inventory unchanged; dry-run parity; idempotent re-import.
3. One end-to-end fixture proves a written octet session reopens with `octet
   --continue`-class resume semantics, and one adversarial fixture proves a
   malicious transcript cannot execute anything or change trust.
4. The behavior is documented in this page and `docs/pi-migration.md`, and the
   import result schema gains an explicit session count/limit field if sessions
   are added to `MigrationImportResult`.

## Not claimed

- No Pi session or transcript has been read or converted here, and no partial
  reader is shipped "for later": an unvalidated parser would be worse than none.
- No session/tree/compaction *mutation* parity, no live Pi session handoff, no
  credential/model-store access, and no change to the existing settings/model/
  skill/MCP import path.
- The unrelated gates stay closed: persisted project trust, host-brokered
  OAuth/credentials, clipboard image capture, rg/fd auto-download, and the
  chord/CBOR/unix-socket architecture.
