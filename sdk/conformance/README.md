# Shared typed-value fixtures (v1)

`typed-values-v1.json` is consumed by the Python and Rust SDK tests and the
production-host conformance harness. Its record is equivalent to a closed
record with `name: string`, `enabled: boolean`, `samples: list[number]`, and
`note: optional[string] = null`. Compare schemas semantically, ignoring only
annotation keys (`title`, `description`, `$schema`) and ordering of `required`
and `anyOf`. Do not relax constraints to make generators agree.

## Authoring simplicity: Pi as the reference

Use the existing runnable examples as a quick author-facing check, not a new
benchmark or release gate. Compare the same task with Pi: ordinary tool,
typed tool, or agent creation. Count required concepts and non-domain setup
lines—not generated code, compressed formatting, or model output tokens.
Declare each type/schema/resource slot once. Ordinary tools must not pay for
resource/bulk machinery. Add a helper only when it removes repeated work or
makes the example clearly simpler; otherwise use the existing API. Keep setup,
cleanup and failure handling visible so short examples remain honest. This is
an implementation tie-breaker, not grounds for unrelated API rewrites or delays.

## Diagnostic wire shape

`metadata.octet_diagnostics_v1` is a list of closed diagnostic records. It is
optional; ordinary/Pi results without it are unchanged. Fields:

- Required: `severity` (`error|warning|info|hint`), `code` (1–128 ASCII,
  leading letter then alphanumeric/`_.-`), `message` (1–4096 UTF-8 bytes).
- Optional: `primary: Location`, `related: list[Related]`,
  `fixes: list[Fix]`, `attachments: list[Attachment]`. Absent lists mean empty;
  absent primary means no location. Optional fields may be omitted, not null.
- `Location = {source: Source, span: {start_byte: uint, end_byte: uint}}`.
  Byte offsets are zero-based, half-open, ordered, at most 2^53−1.
- `Source` is exactly one of:
  `{kind:"workspace", path:relative_path, revision:sha256_hex}`,
  `{kind:"blob", id:opaque_id}`, `{kind:"artifact", id:opaque_id}`.
  Workspace paths are normalized nonempty forward-slash paths (no empty,
  `.` or `..` components, leading slash, backslash or colon). Revisions are
  64 lowercase hex bytes. Immutable blob/artifact identity supplies revision.
- `Related = {message:string, location:Location}`.
- `Fix = {title:string, edits:nonempty_list[Edit]}`; `Edit = {location:Location,
  replacement:string}`. Edits must target revision-bound **workspace** sources,
  never blobs/artifacts. A fix describes edits; it does not apply them or grant
  filesystem authority.
- `Attachment = {kind:"blob"|"artifact", id:opaque_id, label?:string}`.
  References identify existing authorized objects; diagnostics do not create,
  retain or grant access to them. Full BlobRefs remain in typed domain outputs.

All nested records are closed. Bounds: 32 diagnostics, serialized diagnostic
list ≤64 KiB; 16 related locations, 8 fixes/diagnostic, 16 edits/fix,
16 attachments/diagnostic. General display strings are ≤4096 UTF-8 bytes;
fix replacements ≤16 KiB; opaque IDs are 1–128 printable ASCII bytes without
spaces. Display strings permit newline/tab but no other Unicode control
characters (C0/C1).
Workspace paths are ≤4096 bytes and permit no controls. Empty replacements
are allowed; messages/titles/labels are nonempty.

Bounded text projection is one line per diagnostic:
`severity[code]: message` (message newlines/tabs become spaces), at most the
first eight diagnostics and 4096 UTF-8 bytes in total. It supplements structured
metadata, not the existing error envelope, and must not contain terminal
control characters. The SDKs use this same projection. Source verification,
reference grants and result admission remain host responsibilities.

## Evidence boundaries

Raw SDK subprocess tests are not production-host acceptance. The latter must
execute the SDK processes through `ExtensionProcess`, assert invalid calls
never enter handlers, and check structured output, progress and cancellation.
Missing built native binaries are a hard failure of an explicitly requested
conformance run, not a successful skipped acceptance.
