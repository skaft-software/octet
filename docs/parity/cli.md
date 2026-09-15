# CLI parity (rows 5.1 - 5.11, roadmap #180, #313)

Reference (read-only): `earendil-works/pi` at
`8a7b0c03dfb702663acafb6dc29f8acaa4ffe391` (v0.85.1+72). No upstream source is
vendored; every behavior below is implemented in octet's own crates.

Implementation anchors:

- `crates/octet-coding-agent/src/cli/parity.rs` — additive options, `--models`
  scoping, `@file`/stdin expansion, credential-filtered model listing, session
  identity/name selection.
- `crates/octet-coding-agent/src/cli.rs` — flag surface (`--list-models`,
  `--session-id`, `--name`, `--no-session`, `--models`, `--mode json`,
  `additional_messages`, `serve --name`).
- `crates/octet-coding-agent/src/lib.rs` — dispatch order
  (`validate` -> `prepare_input` -> `build_config` -> `list_models` ->
  `resolve_models` -> `select_session`).
- `crates/octet-coding-agent/src/modes/rpc.rs` — `JsonEventStream`, the delta-only
  session-event JSONL projection.
- `crates/octet-coding-agent/src/modes/print.rs` — one invocation, sequential
  prompts, media parts, JSON streaming.
- `crates/octet-coding-agent/src/modes/export_html.rs` and
  `crates/octet-coding-agent/src/session_commands.rs` — `sessions export
  --format html`.
- `crates/octet-coding-agent/src/cli/config_diagnostics.rs` — #313 extraction.
- `crates/octet-coding-agent/src/cli/catalog_publish.rs` — `octet catalog publish`
  with its fail-closed gates (row 5.10).
- `crates/octet-coding-agent/src/codex_context.rs` — the Codex context-window
  policy plus the `--codex-context-window` opt-in override entry point
  (`resolve_codex_context_window`); `src/cli/parity.rs` parses and validates the
  CLI flags.

Behavioral coverage lives in
`crates/octet-coding-agent/tests/parity_cli.rs` (process boundary, isolated
HOME/workspace/session root, loopback fixture only) and
`crates/octet-coding-agent/tests/configuration_diagnostics_full.rs` (#313).

| Row | State | Behavior and evidence |
| --- | --- | --- |
| 5.1 `--mode json` session-event JSONL | Verified | `--mode json` selects `Mode::Print`; `JsonEventStream` writes a session header first, then `agent_start`/`turn_start`/`message_start`/`message_update`/`message_end`/`agent_end`. Cumulative snapshots are removed from delta records, matching upstream's delta-only JSONL. Print mode consumes the first combined stdin/files/prompt and then each remaining positional prompt sequentially. RPC keeps exclusive ownership of stdin JSONL: positional prompts in `--mode rpc` fail closed. |
| 5.2 `--list-models` optional search | Verified | Listing is credential-filtered through `model_catalog_with_offline`, sorted by `(provider, model)`, and filtered by a case-insensitive token-conjunctive fuzzy search. An unmatched search prints `No matching available models.` |
| 5.3 `--session-id` and `--name` | Verified | `--session-id` is exact workspace-local identity: it creates the transcript when missing (materializing the workspace-scoped store directory first) and otherwise resumes it. `--name` is trimmed, rejects empty/control/oversized values, and persists through `SessionStore::rename`. `--name` with a session picker or `--fork` fails closed rather than guessing a target. |
| 5.4 `--no-session` ephemeral | Blocked | Conversation entries and durable usage/`usage_uncertainty` accounting share one locked `Session` JSONL (`octet-agent` `Session`). An ephemeral mode that deletes that log would lose durable accounting, which the brief forbids. The flag therefore fails closed with the exact missing primitive: an accounting-only `Session`/usage backend that survives conversation deletion. |
| 5.5 `@file`/media expansion and sequential prompts | Verified | `@path` reads a bounded regular file (5 MiB per file, 20 MiB combined with stdin) through `octet_agent::secure_fs::read_regular_file_bounded`; recognized inline raster images become bounded `Media` parts (at most 8), everything else must be UTF-8 text and is wrapped in an inert `<file name="...">` element. Remaining positional prompts run in order. |
| 5.6 piped stdin in every mode | Verified | Every non-RPC frontend consumes one bounded UTF-8 stdin prompt and prepends it to the first message; redirected stdin forces print mode unless `--plain` or `--mode json` was explicit. RPC keeps stdin and rejects positional prompts. |
| 5.7 `--models` glob cycling constraints | Partial (verified) | Patterns are comma-separated, trimmed, non-empty, control-character-free, and may carry a trailing `:level` that is only stripped when it names a real reasoning level. A pattern matches `provider/model` or the bare model id, case-insensitively, with `*`/`?` globs (upstream `resolveModelScopeFromModels` rule); matches are deduplicated in first-seen pattern order and sorted by canonical id, because octet's catalog is a map and the selected default must be stable. An unmatched pattern warns and keeps the rest of the scope; an empty pattern list fails closed. The first scoped model (and its `:level`) becomes the default only for a new session with no explicit `--model`. Blocked remainder: ordered cycling itself is owned by the TUI scoped-model surface (`src/modes/interactive.rs`, row 2d.9), which has no scope input today. |
| 5.8 single-file HTML export | Verified | `sessions export --format html` renders one script-free document: sanitized text, an escaping CSP (`default-src 'none'; img-src data:`), bounded inline raster images re-encoded from decoded bytes, inert markdown, and both `light`/`dark` theme palettes. URLs, SVG, audio and unrecognized payloads are never fetched or embedded. |
| 5.9 incremental session/entry search | Verified | `octet sessions search <QUERY> [--limit N]` searches bounded user/assistant entry text through a disposable SQLite projection (`indexed_entries`) added to the workspace catalog (schema 4). Each session stores its transcript fingerprint at index time, so a search re-reads only sessions whose fingerprint changed; unchanged sessions are answered from the catalog and a repeat search re-reads nothing. Only `{"Text": …}` parts are indexed — reasoning, tool calls/arguments, media and provider metadata are never retained. Change notification is the monotonic `catalog_meta.entry_revision`: `apply_entries` advances it only on a real change and `SessionSearchWatcher::observe` fires exactly once per advance. Unit tests assert the incremental scan counts (cold = 2, warm = 0, changed = 1) and the notification; the process-boundary test asserts the same through the CLI. |
| 5.10 catalog publish gates | Verified | `octet catalog publish <DOC> --destination <PATH> --min-client-version <SEMVER> --require-provider <ID> --expected-count <N> --expected-checksum <HEX>` validates five fail-closed gates before touching the destination: sha256 of the exact source bytes, the `octet-catalog-1` schema, the declared-vs-expected minimum client version (refused when the document requires a newer client), the required-provider set (refused when a required provider is absent from the build's built-in providers), and the entry count. The path is immutable: `install_immutable` writes a private temporary in the destination directory and `hard_link`s it into place, so an existing catalog (or a concurrent publisher) refuses the publication and the previous bytes are byte-for-byte untouched. Seven unit tests plus one process-boundary test cover each gate. |
| 5.11 model-backed eval harness | Not started | Needs an owning primitive: there is no isolated eval runner or artifact-store boundary in the coding-agent crate, and the brief forbids unsolicited paid/live inference. A harness must own its own provider injection, artifact directory, and pass/latency/cost delta recording before it can land. |
| #180 `octet serve --name` | Verified (CLI side) | `serve` accepts `--name <NAME>` and the dispatch calls the Serve-owned `run_with_session_name(config, port, no_open, web_root, name)`, which trims, bounds to 120 characters, rejects control characters and treats blank input as "no name". A build without the embedded `serve` runtime fails closed instead of silently dropping the name. |
| #313 configuration diagnostics | Verified | Diagnostics live in the private module `src/cli/config_diagnostics.rs` with no behavior change: identical layer precedence, bounded/secure reads, missing-file handling, accepted aliases/ignored keys, deterministic ordering, source/location reporting, warning/error routing and `OCTET_STRICT_CONFIG` handling. `tests/configuration_diagnostics_full.rs` remains the process-boundary contract. |
| Codex context-window override (user-requested) | Verified (CLI side) | `--codex-context-window <TOKENS>` is an explicit opt-in that only ever *raises* the deliberate 272K Codex cap; `--codex-context-window-acknowledge-cost-cliff` acknowledges the double-priced cliff (~2x input / 1.5x output for the whole request) and the increased websocket-drop risk. Values ≤ 0, above the largest entitlement (1M), or above the cap without the acknowledgement fail closed at validation. The value is resolved through `crate::codex_context::resolve_codex_context_window`, which requires a Pro/ProLite entitlement above the cap and refuses above-entitlement requests; above the cap the decision sets `has_uncertain_usage`, so cost/usage is never presented as exactly known. Without the flag the deliberate default is unchanged. The 272K cap itself is a deliberate product decision (websocket stability, OpenAI guidance, double pricing), not a defect. |

## Non-negotiable exclusions honored

No persisted project-trust change, no host-brokered OAuth/credential-policy
change, no clipboard image capture, no rg/fd auto-download, no chord/CBOR/
Unix-socket work. `--telemetry` JSONL output is untouched. Media keeps octet's
explicit admission limits and durable usage accounting.

## Changelog-ready bullets

- `octet --mode json` now streams session events as delta-only JSONL
  (session header first) instead of requiring RPC framing.
- `octet --list-models [SEARCH]` lists the credential-filtered model catalog with
  an optional fuzzy search.
- `octet --session-id <ID>` creates or resumes one exact workspace session, and
  `octet --name <NAME>` sets its display name.
- `octet --print` accepts `@file` (text and bounded inline images) plus piped
  stdin, and runs multiple positional prompts sequentially.
- `octet --models <PATTERNS>` scopes model selection to `provider/model` globs
  from the credential-filtered catalog.
- `octet sessions export <ID> --format html` writes a single self-contained,
  script-free session document.
- `octet serve --name <NAME>` names the session created at startup.
- `octet catalog publish` validates a catalog document's checksum, schema,
  minimum client version, required providers and entry count, then installs it at
  an immutable path — refusing to replace an existing catalog.
- `octet --codex-context-window <TOKENS>` deliberately raises the 272K Codex
  context cap as an acknowledged, entitlement-checked opt-in; above the cap usage
  is reported as uncertain because the whole request is double-priced.
- `octet --no-session` is intentionally unavailable until an accounting-preserving
  ephemeral backend exists; it never silently drops durable usage accounting.
