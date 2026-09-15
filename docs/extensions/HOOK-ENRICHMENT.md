# Retained API 0.2 hook enrichments

These are maintenance interfaces for existing API `0.2` processes and native
Rust extensions. They are **not API `0.3` methods** and do not change frozen API
`0.1`. New authoring uses the [current guide](../extensions.md) and
[generated contract](API-0.3-REFERENCE.md). Ordinary hook framing is in the
[legacy protocol reference](PROTOCOL-REFERENCE.md#14-hookrun).

## Bounded progress decorations

Negotiate both `request_progress` and `progress_decoration`. During an active
request, send ordinary `$/progress` with a strictly increasing sequence and:

```json
{"type":"decoration","label":"Indexing","detail":"Reading selected resources"}
```

`label` is nonempty, at most 256 UTF-8 bytes. Optional `detail` is at most
4096 UTF-8 bytes. Control characters (including ANSI escape) are rejected.
The retained Python helper is `ext.progress_decoration(label, detail=None)`;
it requires an active parent and both features. The native sink is
`ToolProgressSink::decoration`.

A decoration is one replaceable semantic annotation, not rendering code or a
new durable result. The host's 64-message nonblocking progress channel can drop
updates under pressure. Inactive, late, and non-monotonic progress is ignored;
malformed active progress is rejected. Decorations never change final tool
output, session evidence, or provider context.

## Namespaced pre-persistence metadata

Declare `before_persistence` in `contributes.hooks`. The hook receives completed
assistant-turn facts: `run_id`, `model`, `protocol`, `stop_reason`, `text_bytes`,
`tool_call_count`, `reasoning_part_count`, and `media_part_count`. It does not
receive assistant text, reasoning contents, tool arguments, or a mutable
session. Its execution context carries the host-owned resource-owner fence.

Return an optional `persistence_metadata` object with `value` (inert JSON) and
`public` (boolean, defaults to false). The Python helper is
`persistence_metadata(value, public=False)`. Do not supply a namespace or
provenance: the host attaches the registered manifest name and process
generation. Native `PersistenceMetadataHook` registrations use an explicit
host-selected namespace; invalid or duplicate registrations fail Agent creation.

The host collects hooks within **one aggregate 200 ms deadline**. Cancellation,
timeout, malformed values, and absent proposals cannot veto persistence.
Metadata is sanitized and appended atomically beside the canonical assistant
message. Host-owned display text, synthetic-message markers, usage, tool results,
and other fields cannot be replaced through this interface.

Per entry: at most 32 namespaces, 16 KiB encoded JSON per value, 128 KiB encoded
values in total, depth 16, 256 JSON nodes per value, and 256-byte object keys.
Keys and strings cannot contain control characters. Namespace segments are
lowercase ASCII identifiers separated by dots (128 bytes total, 64 per segment).
Provenance must match the namespace. Invalid values are omitted, not truncated
into a different JSON value. Private metadata remains durable but is absent from
`EntryMetadata::public_extension_metadata()`. All metadata stays out of provider
context; frontend/export integrations must opt into the public-only projection.

## PostMutation observations and rescans

Declare `post_mutation` in `contributes.hooks`. Only the host emits these after
successful commit or completed rollback, never for previews or partial writes:

```json
{"mutation_id":"mutation:one","kind":"resource","affected_resources":["resource:one"],"generation":1,"state":"committed"}
```

Kinds are `configuration`, `resource`, and `migration_ingestion`; states are
`committed` and `rolled_back`. The shape is content-free: identifiers are opaque
lowercase ASCII host identities, not paths, credentials, or file contents. IDs
are at most 128 bytes, the affected list at most 32 resources, and generation
must be positive. Lists normalize to sorted unique identities.

Return `post_mutation: {"action":"no_rescan"}` or
`post_mutation: {"action":"request_rescan","resource_ids":[...]}`. The Python
helper `post_mutation_rescan(resource_ids)` constructs the latter. Requests must
select a nonempty bounded subset of the disclosed affected list. The hook cannot
supply paths, request a watch feed, replay the mutation, or authorize effects.

The coding host dispatches concurrently with a 250 ms deadline per process,
discards responses from replaced process generations, remembers the most recent
256 mutation IDs across process reloads within the current host instance, and
retains at most 256 validated rescan requests. Duplicate IDs in that window are
not delivered again. Owner changes discard pending rescan requests.

**Current product integration:** successful isolated and runtime-manager-backed
extension reloads both emit resource observations. At App catalog reconciliation
(including the interactive reload boundary and before prompting), the host drains
the queue, coalesces selected extension resources, rejects stale generations,
and re-resolves them through the same precedence, trust, regular-file/no-follow,
and byte-limited manifest reader as startup. This is a real read-only rescan:
changed or unavailable sources are diagnosed and require explicit `/reload`;
it never activates code, grants trust, or recursively reloads a process.

General configuration writes and committing migration ingestion do not yet emit
these observations or map their resource families into the product consumer.
The typed Rust bridge exists, but calling it is not proof of product integration.
Dry-run migration must never emit a mutation notification.
