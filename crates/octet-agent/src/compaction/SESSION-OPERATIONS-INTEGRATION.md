# Session operations implementation integration — 2026-10-04 15:51Z

Code is written directly in this tree. Worker owns these modules and is adding tests; parent owns the glue below. No Cargo run in worker.

## Exact required shared glue

`crate::compaction` now publicly exports `SessionOperationHook`, `SessionOperationInvocation`, `SessionOperation`, `SessionOperationDecision`, `SessionCompactionReplacement`, `SessionCompactionReason`, `SessionOperationError`, `SessionSourceRevision`, and `run_session_operation_hooks`.

`ExtensionProcess` **already implements** `crate::compaction::SessionOperationHook` in `extension_process/session_leaf/session_operations.rs` (registered from the owned `session_leaf.rs`). No new lib.rs module registration needed.

In `ExtensionHost` (`extension.rs`):

```rust
pub(crate) session_operation_hooks: Vec<Arc<dyn crate::compaction::SessionOperationHook>>,
// Default/new: session_operation_hooks: Vec::new(),
// Add public registration:
pub fn session_operation_hook(&mut self, hook: impl crate::compaction::SessionOperationHook + 'static) {
    self.session_operation_hooks.push(Arc::new(hook));
}
// scoped clone: scoped.session_operation_hooks = self.session_operation_hooks.clone();
```

Add `ExtensionHook` enum variants **SessionBeforeCompact, SessionCompact, SessionBeforeTree, SessionTree** (serde already snake_case). They are NOT `is_session_hook` lifecycle Start/End variants. They require API 0.4 and `session_entries`. At codinghost process registration, call `host.session_operation_hook(process.clone())` when the process declares any of those four variants. Reuse the existing hook admission checks. The process trait filters subscriptions per event.

All **three** `CompactionContext` constructions in `agent/turn_loop.rs` need:

```rust
session_operation_hooks: &extension_host.session_operation_hooks,
```

The construction in `agent/compaction.rs` is already updated by worker. No `Agent` field needed. No `ExtensionHookOutput` field needed: the owned leaf lane removes/decodes `session_operation`, then strictly decodes the ordinary envelope and rejects unrelated hook effects.

## Existing idle-driver integration

The implemented method is:

```rust
Agent::compact_session_with_instructions(
    &mut self,
    instructions: Option<&str>,
    cancellation: octet_agent::CancellationToken,
    on_event: impl FnMut(octet_agent::AgentEvent),
) -> Result<octet_agent::CompactionInfo, octet_agent::AgentError>
```

Bind lifecycle `Compact { instructions: Option<String> }` to this method on the existing idle owner. It invokes before hooks, honors veto, supports validated textual replacement, or runs the existing hard-budget/retry/accounting summary path, commits once, then runs session_compact with the append lane. Do not ACK from queue admission. Local mode only; native-responses mode explicitly refuses this cancellable manual service. Existing `compact_responses_native` separately now intercepts/observes too.

To return `{entry_id,summary,first_kept}`, use the actual Compaction entry added since the pre-call entry count (after hooks may append non-context entries, so session.head is NOT necessarily compaction ID). Return errors, including explicit veto, to the operation callback. Post-commit hook failures have an error string naming the real committed ID and forbidding retry; the entry is not undone.

`Agent::navigate_session_tree(target: Option<EntryId>, cancellation: CancellationToken) -> Result<(), AgentError>` is also implemented. Existing idle tree driver can use it instead of raw checkout. Before hooks may veto; after hook runs only after synced checkout. Branch summary replacement is explicitly NOT supported; never advertise it from the presence of these names.

## Adapter wire

Awaited `hook/run` names: `session_before_compact`, `session_compact`, `session_before_tree`, `session_tree`.

Payloads:
- `{"kind":"before_compact","reason":"manual|threshold|overflow","first_kept":"...","preparation":{"messages":[native Message],"turn_prefix_messages":[],"previous_summary":null,"details":{"readFiles":[],"modifiedFiles":[]}},"branch_entries":[native Entry],"custom_instructions":null}`.
- `{"kind":"compacted","reason":"...","entry":native Entry,"from_extension":bool}`.
- `{"kind":"before_tree","target_id":string|null,"old_head":string|null}`.
- `{"kind":"tree","old_head":string|null,"new_head":string|null}`.

Reply ordinary envelope plus `"session_operation":{"action":"continue"}` / `{"action":"cancel"}` / `{"action":"replace_compaction","replacement":{"summary":"...","first_kept":"..."}}`. Replacement unknown fields refuse; opaque Pi details/usage are not claimed. `disposition:{action:"deny",reason:"..."}` maps to veto. Observations cannot cancel already committed work.

The private `session_leaf` wire remains unchanged and receipt-based. Each operation callback includes actual full native `context.host.session_entries`, `session_branch`, `session_leaf_id`, `session_file` snapshots, never fabricated Pi entries. `session_id` stays the existing host context ID. Session paths are native format, not Pi-file ABI.

Provider-context owner can obtain the same mirror by changing its bound lease construction to `.bind_session_leaf(...).and_then(|lease| lease.with_session_snapshot(session))`. That helper is already public in `extension_process/session_leaf.rs`; worker did not touch provider_context files.

## 15:58 integration / privacy update

All worker Rust files are syntactically rustfmt-checked and ready for parent compilation. Shared glue has now been observed in `extension.rs`, `manifest.rs`, and all three turn-loop constructions. Worker never edited those parent-owned paths.

**Metadata privacy fixed:** `session_entry_for_namespace` in the owned process leaf module filters every mirror entry AND operation `branch_entries`/committed `entry` to public metadata plus only the receiving manifest namespace's private values. It retains the original records untouched. Per-value 16KiB, aggregate 128KiB, namespace-count bounds are checked and fail closed; complete request snapshot serialization is bounded by the actual connection max-message limit, without truncating entries. New tests cover private cross-namespace exclusion and oversize refusal.

Actual tree frontend change points: `extensions/serve/runs.rs` `SessionCommand::Checkout` currently calls `owned_app.agent.session_mut().checkout(EntryId(...))`; use the new awaited Agent method before rebuild. `extensions/serve/checkout.rs::checkout_before_user_entry` is a lower-level Session-only branch mutation used by other transitions and cannot invoke Agent hooks itself. `modes/interactive.rs::restore_session_head` is test-only; do not mistake it for the live boundary. Rebuild/rollback logic needs to preserve committed-vs-refused outcomes after hooks.

`before_agent_start` is not covered by this lane. Actual current prompt pipeline is `extensions/turns.rs::compose_prompt`, which invokes `BeforePrompt` with `before_prompt_hook_payload(&prompt)` and processes context contributions. It has the real base system text, but system replacement/custom-message persistence require their own typed output and real admission/commit consumers; do not alias it to an observation. Provider-context owner must make any effective system replacement reach the actual request.

## Verification / limits

Core tests in `compaction/session_operations/tests.rs`; seven agent integration tests in `agent/compaction/session_tests.rs`; two metadata snapshot tests in the process leaf module. No compilation/test execution claimed yet (parent owns Cargo). Native branch-summary records/replacement, replacement-session setup handles, callback-operation cancellation receipts, and arbitrary Pi compaction details remain incomplete. Durable append bounds remain 16KiB / 256 nodes; no silent widening. No default provider fallback after veto/error.

## Follow-up: live frontend wiring — 2026-10-04

The coding frontend's `compaction.rs` now delegates local manual compaction to
`Agent::compact_session_with_instructions`; the duplicate summary/commit path
is removed. Forced retention reaches the Agent and its original policy is
restored after settlement. Interactive cancellation cancels and settles the
owned future instead of dropping it, and reporting follows the active ancestry
past metadata appended by after-hooks. Native Responses keeps its separate API.

Serve's live `SessionCommand::Checkout` now awaits `navigate_session_tree`.
A veto with no mutation retains the owner; a failed hook after mutation retires
the owner for reopening rather than rolling back durable work or reusing an
old-branch model. Existing guarded rebuild/rollback paths remain.

Targeted frontend tests are authored but **not compiled or run**. The performance
agent now owns the heavy-build slot; only source review, rustfmt and whitespace
checks ran for this follow-up. Earlier driver tests do not qualify these callers.

## Branch-summary integration — 2026-10-09

The dated notes above describe earlier integration states. Native tree navigation
now also supports `Agent::navigate_session_tree_with_summary(target, summarize,
custom_instructions, cancellation, on_event)`. It returns `TreeNavigationResult`
with optional `editor_text` and `summary_entry`. Selecting a user/custom entry
restores its text for editing and navigates to its parent; it never resubmits it.

Summary preparation includes only the abandoned span to the common ancestor.
`before_tree` can include `preparation` with `common_ancestor_id`, native
`entries_to_summarize`, `user_wants_summary`, and `custom_instructions`. The
committed `tree` event can include the real native `summary_entry`. Absent optional
fields retain the ordinary checkout wire contract. Existing metadata namespace
filtering applies to both preparation and committed observations.

The summary entry and selected head persist together after validation. Provider
failure or cancellation before commit does not move the branch; post-commit hook
failure discloses the durable outcome and forbids retry. Branch summaries retain
IDs, parent links, timestamps, `from_entry` provenance and file details across
replay/fork. They contribute custom context, not resurrected user prompts or
extension authority. Arbitrary extension branch-summary replacement remains
unsupported; compaction replacements keep their separate typed contract.

Parent verification covers the agent library and native frontend tests. These
synthetic checks do not qualify existing Pi packages, attended terminals, real
providers, installers or release artifacts.
