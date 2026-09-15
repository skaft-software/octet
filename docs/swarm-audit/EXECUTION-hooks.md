# Extension hooks execution — df5a7e80 + working diff

Scope: #267 PostMutation, #264 progress decoration, #265 pre-persistence metadata; qualification of #41 #47 #254 #255 #263 #268 #269 #270 #271. No commits or shared Git-state changes.

## Initial inspection

- Read complete extension guide, current API 0.3 generated reference, retained protocol/authoring references, Python SDK/runtime guide, and hello-world/local-model-workflow/current minimal example guides. API 0.1 is frozen; these request-path enrichments belong only to retained API 0.2, not API 0.3.
- #267: `ExecutableExtensions::notify_post_mutation` validates selected-resource subsets and queues requests; `take_post_mutation_rescans` has no consumer. Legacy reload emits observations, but manager-backed reload returns before notification (the normal product path). Configuration and committing migration App paths are outside this worker's write ownership; final wiring requirements will be listed here.
- #264/#265: host and SDK implementations exist but dedicated proving tests are missing.
- Tests not yet run.

## Work in progress / incremental evidence

- Fixed manager-backed reload skipping PostMutation. Both ownership modes now share post-success notification and shortcut-refresh logic.
- Added real retained-SDK subprocess hook fixture and product tests for commit/rollback, duplicate delivery across reload, subset/malformed rejection, timeout, generation fencing, and bounded filesystem rescan consumption.
- Parent reassigned `src/app/mod.rs`: catalog reconciliation now drains rescan requests through the existing resource resolver. Only current-generation extension resources are read; no effects, activation, trust changes, or recursive reload. Changed resources remain an explicit `/reload` decision. General configuration/migration resource families remain unimplemented and are not claimed.
- Added Rust Agent/Session and SDK tests for progress decorations and metadata: byte/control bounds, backpressure, private/public projection, host provenance, host-field stripping, durability, no provider-context contamination, and the aggregate non-veto hook deadline.
- `cargo test -p octet-coding-agent --lib extensions::hook_tests -- --nocapture`: initial 2/2 passed before rescan-consumer test was added. Existing coding crate lint warnings plus the then-unfulfilled queue `expect(dead_code)`; consumer implementation removes that expectation.
- `cargo test -p octet-agent --test agent_run extension_hooks -- --nocapture`: initial 1/3 passed; two new test fixtures dropped their TempDir prematurely (not product failures). Fixed fixtures to retain ownership; rerun queued.
- `PYTHONPATH=sdk/python python3 -m unittest discover -s sdk/python/tests -p 'test_extension_hooks.py' -v`: 4/4 passed.
- Running full focused qualification chain in existing Cargo target; transient local log `/tmp/octet-hooks-qualification.log` (not a committed artifact). Includes requested recovery_current/api_v03_runnable and implemented API ledger targets. Parent may run aggregate verification after writers settle.

## Final results for this slice (df5a7e80 + shared working diff)

### Implemented and qualified

- **#264**: new SDK tests plus Rust ingress and sink tests prove feature gating, parent correlation, monotonicity/late-drop behavior, UTF-8/control bounds, and 64-slot backpressure. The real SDK subprocess runs inside the Agent loop; decorations reach live events but not durable final results or subsequent provider requests. Evidence: `crates/octet-agent/src/extension_process.rs:14963`, `crates/octet-agent/tests/extension_hooks.rs:130`, `crates/octet-agent/tests/support/extension_hooks.rs:152`, `sdk/python/tests/test_extension_hooks.py:9`.
- **#265**: native and process metadata hooks are exercised at the actual assistant persistence boundary, including restart/reopen, private/public projection, namespace provenance, invalid/duplicate registration, malformed/control/oversized/deep/node/aggregate rejection, stripping host fields, no provider-context contamination, and aggregate 200 ms non-veto timeout. Evidence: `crates/octet-agent/src/extension.rs:1007`, `crates/octet-agent/tests/extension_hooks.rs:56`, `crates/octet-agent/tests/support/extension_hooks.rs:30` and `:119`, SDK test `test_before_persistence_preserves_private_default_and_cannot_supply_host_provenance`.
- **#267 targeted ledger gap**: manager-backed and isolated reloads both deliver PostMutation; App reconciliation consumes the typed queue through real bounded read-only resource discovery. Tests cover commit/rollback, subset rejection, malformed and late responses, duplicate IDs across reload, source/generation changes, owner-change queue clearing, 256-entry bounds and coalescing. Evidence: `crates/octet-coding-agent/src/app/mod.rs:483`, `crates/octet-coding-agent/src/extensions.rs` (`reload`, `rescan_post_mutation_resources`), `crates/octet-coding-agent/src/extensions/hook_tests.rs:80`, `:156`, `:178`, `:228`. A rescan validates the selected on-disk extension descriptor; it does not implicitly activate code, modify policy, or auto-replace a changed runtime. Generic configuration/migration scope remains partial as described below.
- Retained API documentation added in `docs/extensions/HOOK-ENRICHMENT.md`, linked from current/legacy guides and the Python runtime reference. No wire/schema version changed, no generated contracts modified, and no OAuth/credential broker policy changed.

### Observed verification

| Command | Observed result |
| --- | --- |
| `cargo test -p octet-agent --lib` | **527 passed, 1 ignored**, final run after added namespace-registration test. |
| `cargo test -p octet-agent --test extension_hooks` (within multi-target invocation) | **4 passed**. |
| `cargo test -p octet-agent --test agent_run extension_hooks -- --nocapture` | **3 passed** after retaining test TempDirs. |
| `cargo test -p octet-agent --test agent_run retry_hook` | **2 passed**. |
| `cargo test -p octet-agent --test runtime_governance_full` (within multi-target invocation) | **11 passed**. |
| `cargo test -p octet-agent --test extension_api_0_1_conformance` (within multi-target invocation) | **4 passed**, including real API 0.1/0.2 Python subprocesses. |
| `cargo test -p octet-agent --test extension_api_v03_conformance` (within multi-target invocation) | **5 passed**. |
| `cargo test -p octet-agent --test recovery_current` (within multi-target invocation; parent requested #350) | **3 passed**. |
| `cargo test -p octet-agent --test api_v03_runnable` (within multi-target invocation; parent requested #253) | **1 passed**. |
| `cargo test -p octet-coding-agent --lib extensions::` | **40 passed**; existing unrelated coding-crate warnings remain. Initial new rescan test incorrectly used noncanonical macOS temporary paths; canonical fixture paths now match product discovery, and rerun passed. |
| `PYTHONPATH=sdk/python python3 -m unittest discover -s sdk/python/tests -p 'test_extension_hooks.py' -v` | **4 passed**. |
| `PYTHONPATH=sdk/python python3 -m unittest discover -s sdk/python/tests` | **58 passed, 1 failed**: existing `test_previous_import_name_is_not_a_source_alias` sees an ignored, untracked `sdk/python/ygg_extension/__pycache__` directory as a Python namespace package. No SDK runtime implementation was changed. Residue was left untouched. |
| Same complete Python suite in a temporary clean source copy of `octet_extension/` and `tests/`, using unchanged repository protocol fixtures | **59 passed**. Confirms the checkout-only namespace residue cause without changing or suppressing the test. |
| `python3 scripts/generate-extension-api-v03.py --check` | **passed**, generated contract unchanged. |
| Scoped `git diff --check` | **passed**. Only new files and changed snippets were formatted; no global formatting or Git-state operations. |

The initial asynchronous aggregate log stopped during compilation without a terminal result and is **not** used as aggregate evidence. Subsequent synchronous commands above completed. Temporary command logs: `/tmp/octet-hooks-qualification-agent.log`, `/tmp/octet-hooks-final-agent-lib.log`, `/tmp/octet-hooks-qualification-product.log`, `/tmp/octet-hooks-retry.log`, `/tmp/octet-hooks-python.log`; this Markdown is the durable execution artifact. No additional octet artifact/session identifier was exposed.

### Existing implemented-row qualification

- **#41**: coding shortcut registration/reserved-first-owner and process dispatch tests passed in the 40-test extension suite; agent process shortcut bounds/initialize matching tests passed in the library suite.
- **#47**: API 0.3-only typed flag validation/default delivery tests passed in the agent library; coding flag enablement/exact-trust tests passed. No API 0.1/0.2 initialization field changed.
- **#254/#255**: runtime manager binding/sharing, lazy discovery, aggregate process/FD/byte/startup budgets, cancellation/reload reservation ownership and cleanup tests passed (`runtime_governance_full`, 11 tests, plus library tests). App manager-backed reload path is additionally exercised by the new PostMutation fixture.
- **#263**: stop/bounded-delay and deadline-preemption integration tests passed (`agent_run retry_hook`, 2 tests). Hooks still cannot authorize replay or expand host budgets.
- **#268**: conditional session-lifecycle offer, bounded epoch-fenced driver, and canonical dispatch tests passed in the agent library; interactive-only eligibility tests passed in the coding extension suite.
- **#269**: lifecycle-owned provider replacement, stale cleanup, inactive changed routes and complete startup batches passed (`extension_provider` library tests).
- **#270**: bounded stream ingress and route revalidation during acceptance remain implemented; relevant existing process library tests passed. No live provider replay/stream behavior was claimed.
- **#271**: host-owned authorization/opaque-lease seam remains unchanged; secret-free declarations and unauthorized/secret-field rejection library tests passed. No live OAuth, credential issuance, or remote account action was attempted or qualified.

### Exact remaining integration / ownership boundaries

- The original #267 queue-consumer/test gap is fixed for **extension resource reloads**. The broader feature still has **no configuration-write or committing-migration PostMutation producer**; do not label those families end-to-end implemented.
- Configuration persistence still occurs in unowned `src/modes/interactive.rs` (`persist_reasoning`/`persist_reasoning_mode` at successful idle transitions). It needs a host-created stable mutation ID and positive resource-generation fence, notification only after successful durable write/completed rollback, and an explicit host mapping for the affected configuration resource family. Failed writes and previews must not notify. App rebuild is not itself proof of a durable configuration mutation.
- Committing ingestion and restore are in unowned `src/migrate/migration_import.rs` (`apply_ingestion_plan`, `restore_backup`). These early-exit CLI paths have no safely bound extension observation owner. They must not start extensions merely to report a mutation, and dry-run scans must not notify. `notify_migration_ingested` remains an unused typed integration seam; its obsolete “no committing ingestion exists” comment was corrected.
- No wiring change is required in `bootstrap.rs` or `interactive.rs` for the completed **extension reload** slice: existing calls to `App::synchronize_extension_provider_catalog` now consume the queue after reload and before requests. No unowned mutation-path edits were made.
- Parent owns aggregate workspace checks and CHANGELOG integration. Newly reassigned `crates/octet-agent/src/tools/**` and `tests/parity_tools.rs` were not edited. No new Pi parity or older 0.84.4 qualification was claimed.
