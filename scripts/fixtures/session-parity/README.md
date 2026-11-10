# Session parity fixture (test-only)

Differential evidence that octet's **native** session semantics match Pi's
`SessionManager`. Never shipped, never a dependency of octet or of
`extensions/octet-pi-compat`.

## What runs

`crates/octet-coding-agent/src/session_parity/` (compiled only under
`#[cfg(test)]`) reads `scenarios/*.json`, replays each operation twice:

* **oracle** — `scripts/fixtures/pi-reference/session-driver.mjs`, which imports
  the *published, unmodified* `@earendil-works/pi-coding-agent` at **1.0.2** and
  **1.5.0** (`pi-session-1-0-2` / `pi-session-1-5-0` npm aliases). It drives the
  real `SessionManager` and records `getEntries()`, `getTree()`, `getBranch()`,
  `getLabel()`, `getSessionName()` and `buildSessionContext()` after every step.
* **native** — octet's real `Session`/`SessionStore` API through the same
  operations, plus a read-only reopen of the file after every step.

Then it imports every Pi-written file through octet's real
`session_commands::import_session` and compares the imported session the same
way. Nothing is mocked; nothing reads `~/.pi` or `~/.octet`.

Install the oracle once:

```sh
npm ci --prefix scripts/fixtures/pi-reference --ignore-scripts --no-audit --no-fund
```

Run:

```sh
cargo test -p octet-coding-agent --lib session_parity --profile ci-test --locked
```

`OCTET_PARITY_SEEDS` (default `1`) and `OCTET_PARITY_SEQUENCES` (default `12`)
size the seeded random run.

## Normalization

Entry ids differ between implementations, so **append order is the identity**:
the *n*-th entry of the Pi session corresponds to the *n*-th entry of the octet
session. Every comparison is expressed in that index space, and scenario names
(`op.as`) resolve to indices, never to ids.

1. **Tree.** All entries in append order, each `{kind, parent}`. `kind` is the
   Pi entry type; octet's tagged `EntryValue` maps onto the same vocabulary:

   | Pi | octet |
   | --- | --- |
   | `message` (user/assistant/toolResult) | `Message` user/assistant/tool-result |
   | `custom` | extension entry (`append_extension_entry`) |
   | `custom_message` | `Message` user plus `metadata.custom_message` |
   | `label` | label *record* (`entry_label`), not a tree entry |
   | `model_change` | `Config { model }` |
   | `thinking_level_change` | `Config { reasoning }` |
   | `compaction` / `branch_summary` | same |
   | `session_info` | sidecar name (`SessionStore::rename`), not a tree entry |
   | `usage`, `context_edit` | not representable (see ledger) |

   Pi `label` entries are collapsed before comparison — children are re-parented
   onto the label's parent — because that is exactly what Pi itself does when it
   materializes a branch (`createBranchedSession`). Labels are compared
   separately as a `{target index: label}` map, which is also how octet stores
   them. `parent` is a parent index or `null`, so shape, forks, roots and
   sibling order are compared, not just membership.

   Payloads are compared per kind: message role and ordered content blocks
   (text exactly, images by MIME type and decoded byte length), assistant
   content parts including reasoning text and opaque signature presence,
   tool-result call id/name/error/content, custom type and data, custom-message
   type/content/display/details, label target and text, model and thinking
   values, and compaction/branch-summary summary text plus kept-entry index.
   Assistant `usage`/`stopReason`/`timestamp` and entry timestamps are **not**
   compared: octet records usage as separate session records and stamps its own
   entry times, and accounting parity is out of this harness's scope.

2. **Selected branch.** The leaf index and the root-to-leaf path as entry
   indices.

3. **Model-visible messages.** Pi: `convertToLlm(buildSessionContext().messages)`
   — the real function the agent passes to the provider. octet:
   `Session::context()`, the exact list its request builder sends. Both are
   normalized to `{role, content blocks}`; octet's coalesced tool-result user
   message is expanded back into one `toolResult` message per result, because
   that is the wire shape both implementations produce and the coalescing is an
   internal octet representation. Tool-result `toolName` is excluded from this
   check (octet does not persist it; the entry payload comparison reports it).

Additionally compared, and reported separately because they are effects rather
than the three required checks: resolved model/thinking selection (Pi's
`buildSessionContext().model/thinkingLevel` versus octet's catalog projection,
the same reader resume uses) and the session name.

Pi's driver reopens every checkpoint from disk before recording it; the harness
asserts the reopened state equals the live state (oracle sanity) and that
octet's read-only reopen equals its live state (octet invariant). A Pi file that
does not exist yet (Pi keeps setup-only sessions in memory) is recorded as
`unpersisted`.

## Import check

Every Pi checkpoint file is imported with octet's real importer into a fresh
store, then the imported session is compared on the same three things. Import
normalization differs in one documented way: Pi `label`, `session_info` and
`usage` entries become octet's opaque marker entries (`Config` with no payload),
so they stay in the tree and keep their position; label *content* is still
compared through the label map.

## Known gaps ledger

`known-gaps.json` is the only permitted form of "known gap". Every mismatch is
identified by `scenario | step | check | fingerprint`; a failing test names the
mismatches that are not in the ledger, and a ledger entry that no longer
matches anything also fails, so a fix must delete its line. `scenario` and
`step` may be `"*"` for a divergence that is identical in every scenario;
`fingerprint` must match exactly, so a *new* divergence never hides behind a
wildcard. `reason` states what differs and why it is accepted for now.

## Bounds

Fixed scenarios and a fixed seed set run by default. The random generator is
deterministic (seeded), prints the minimal failing prefix it finds, and stops a
sequence at the first operation octet refuses: a reference that cannot perform
the operation has no differential meaning after that point. Fixed scenarios
place such operations last so nothing else is skipped.
