# Extension values v1 conformance matrix

Contract: [extension-values-v1.md](extension-values-v1.md). Status: implementation authorized; results MUST be recorded separately from this matrix.

## Execution requirements

Every SDK case runs against BOTH real Python and Rust extension executables using the production Rust ExtensionProcess and ordinary dispatch paths. Host product/projection cases additionally run a real Agent/product turn with a local scripted provider capturing the exact model request; no paid inference. SPICE uses actual ngspice, not a synthetic waveform generator. A missing executable is BLOCKED/FAIL, never a success or silent skip.

Fixtures MUST retain a child-side append-only call/disposal log and process identity. Invalid calls assert that the target log gained ZERO entries. Race cases use explicit fixture barriers (entered, output_ready, allow_terminal) and host admission barriers, not timing sleeps. Tests force both possible linearizations. Each case uses a private HOME/workspace and cleans owned processes/files. No OS-sandbox claim follows from HOME isolation.

Production methods MUST perform actual host validation, writer admission, JSON-RPC exchange, terminal processing and filesystem publication. Fake registries, direct fixture function calls and a synthetic protocol host cannot satisfy these cases. Unit tests MAY additionally cover pure helpers. Evidence includes source identity, SDK identity, exact command, exit status, full bounded logs and skipped/blocked counts.

The implementation MUST give the cases below stable executable test names. All rows are independent except prerequisites stated in the trace. Shared fixtures vary SDK language, not expected semantics. Test assertions are the rightmost column; a command exiting successfully with zero matched tests is not a pass.

## A: typed SDK parity

| Case | Real-process trace | Required oracle |
|---|---|---|
| A01 typed_roundtrip | initialize -> typed record input -> typed record output | Same generated schema and structured value for Python/Rust; explicit text part retained |
| A02 invalid_input | Missing/wrong/extra field; nonfinite/nonportable scalar | No domain handler entry; bounded refusal; healthy next valid call |
| A03 invalid_output | Deliberately violate output type/schema | No successful structured result admitted |
| A04 optional_values | Omitted default, explicit null, present value | Identical schema/codec behavior; absent is not silently null |
| A05 diagnostics | Domain failure with code/location/fix and bounded summary | Structured diagnostic retained; no stderr parsing; malformed diagnostic rejected |
| A06 cancellation | Block handler -> cancel -> terminal -> valid follow-up | One caller terminal; no generation replacement for cooperative handler |
| A07 progress | Negotiated progress -> sequence of statuses -> result | Same semantics; no progress bytes in model transcript; unnegotiated progress refused |
| A08 hostile_transport | Oversize/invalid frames, EOF, duplicate terminal, shutdown | Existing bounded protocol and cleanup guarantees preserved |

## B/C: resources and operation descriptors

| Case | Real-process trace | Required oracle |
|---|---|---|
| R01 lifetime | create -> subsequent operation mutates native non-JSON object -> release -> reuse | Same object across calls; invalidation acknowledged; reuse rejected with zero invocation |
| R02 wrong_type | Valid same-owner token with incorrect nominal type/slot | Type refusal before target invocation |
| R03 fabricated | Random unissued token | resource_unavailable; zero invocation |
| R04 foreign_session | Owner A creates; owner B submits token | Unavailable; no A metadata disclosed |
| R05 foreign_extension | Two live extension instances; transfer A token to B operation | Rejected even with equal nominal type string |
| R06 stale_generation | Save ref, replace generation, submit saved ref | Rejected; new same-type object has different identity |
| R07 queued_release | Fill request slot -> queue use -> release idle resource -> free slot | Queued use revalidates and never enters extension |
| R08 admission_release | Enter use barrier -> release -> terminal -> release | First release resource_busy; disposal not called while pinned; second invalidates |
| R09 cancel_first | Register provisional ref -> cancel barrier -> late successful result | No activation/model reference; execution pin retained until late terminal/kill |
| R10 complete_first | Admit successful result -> then cancel caller | One success disposition; admitted resource remains usable |
| R11 crash | Enter native operation -> terminate child -> supervised restart | Outstanding execution settles unavailable; all prior refs stale; no automatic replay |
| R12 reload | Live ref -> accepted candidate reload | Old ref invalid; old execution drained/cancelled; new generation works |
| R13 failed_reload | Live ref -> candidate fails initialize | Old generation and reference remain usable |
| R14 host_restart | Save transcript/ref -> stop actual host -> start fresh host | Old ResourceRef stale even if generation counter restarts |
| R15 invalid_parent_output | resource/register succeeds -> output schema invalid | Zero activated resources; provisional ref unavailable; cleanup observed separately |
| R16 failed_parent | Register -> domain/RPC error | No successful resource projection; provisional retired |
| R17 cleanup_failure | Release -> disposer reports failure | Reference stays retired; cleanup failed, not completed; no resurrection |
| R18 cleanup_hang | Release -> disposer blocks until host deadline/termination | Reference immediately invalid; cleanup unknown; bounded generation cleanup |
| R19 resource_quota | Fill per-parent/per-generation bounds -> one more registration | Refused before activation; no partial publication; capacity recovered after cleanup |
| R20 provisional_isolation | Block creating parent after register; another call uses/discovers token | Neither use nor discovery succeeds before successful parent admission |
| R21 output_atomicity | Parent exports two provisional refs, one invalid field | Neither reference activates |
| R22 owner_roundtrip | Owner A -> B -> A with same process | Retired A ref never revives |
| R23 all_entrypoints | Repeat invalid-owner/type/stale cases via direct call, registered tool and nested composition | Identical zero-dispatch enforcement |
| R24 descriptor_validation | Invalid pointer, receiver not a slot, duplicate/conflicting slots, unsupported array path | Initialize/dynamic mutation fails atomically; previous catalog unaffected |

## D: host lookup and lazy projection

| Case | Real-process trace | Required oracle |
|---|---|---|
| D01 exact_match | Resource type T; catalog has exact T receiver | Exact operation/path/revision returned |
| D02 wrong_type | T resource; operation expects U or derived-looking T2 | No match; no inheritance or prefix matching |
| D03 foreign_lookup | Foreign/unknown/retired resource lookup | Refusal, not leaked candidate metadata |
| D04 frozen_turn | Capture model snapshot rev N; publish N+1 before model call dispatch | Old call uses N descriptor/handler; next turn sees N+1 |
| D05 hidden_policy | Exact matching operation excluded by host policy | Absent from discovery/projection |
| D06 no_authority | Discover/project -> revoke tool policy -> invoke | Execution refuses; discovery/receiver knowledge confers no authority |
| D07 multiple_resources | Receiver A plus second resource B; supply foreign/busy B | All slots checked/pinned atomically; no partial pins or handler entry |
| D08 hundred_operations | Register 100 operations, 20 matching; lookup default limit | <=8 cards, correct truncation/order; only bounded selected schemas reach model |
| D09 cursor_fence | Page one -> catalog/policy revision changes -> next page | catalog_changed; no mixed revision page |
| D10 receiver_metadata_only | Same args/schema with/without primary receiver marker | Invocation/pinning/effects/results identical; presentation differs only |
| D11 pi_registry | Load unchanged Pi factory alongside resource-aware operations | Pi tools remain registered/active with existing semantics; no annotation requirement |
| D12 selection_order | Repeat baseline lookup over same snapshot | Exact deterministic ID/path order independent of semantic ranking |

## E: immutable bulk

| Case | Real-process trace | Required oracle |
|---|---|---|
| B01 lifecycle | write ticket -> file bytes -> commit -> successful parent -> read -> close/release | Exact verified bytes and metadata; lease lifetime enforced |
| B02 wrong_size | Commit with shorter/longer declared length | No BlobRef published; reservations/partial copy reclaimed |
| B03 wrong_digest | Correct length, incorrect digest | integrity_mismatch; zero published blobs |
| B04 oversize | Finite ticket limit exceeded | Refused with bounded reading/memory; existing media limits unchanged |
| B05 abandon | Allocate/write -> parent error/EOF without commit | No exported ref; scratch/reservation reclaimed |
| B06 cancel_publication | Pause host copy/publication -> cancel -> resume | No externally readable blob; partial storage removed |
| B07 disk_full | Inject ENOSPC in real file copy/commit or durable retention write | storage_unavailable; no partially published record; no reservation leak |
| B08 release_lease | Read -> close mapping -> release -> reuse same lease/locator via broker | Lease rejected; underlying retained BlobRef may obtain a fresh authorized lease |
| B09 no_grant | Known BlobRef presented by unauthorized owner | Refused before locator disclosure |
| B10 extension_restart | Publish retained blob and active resource -> restart extension | Old resource/tickets/leases stale; blob can be read with fresh owner-authorized lease |
| B11 host_restart_retention | Durable retained blob plus ResourceRef -> fresh host with same session store | Blob reverified/re-authorized; ResourceRef stale; no persisted locator |
| B12 immutable_snapshot | Publish -> producer rewrites original scratch file | Blob bytes/digest unchanged |
| B13 unsafe_locator | Traversal, absolute path, symlink substitution, foreign ticket/lease | Refusal; no outside-transfer-area read/write |
| B14 bulk_quota | Exhaust write/lease/retained-byte bounds | Bounded refusal; cleanup restores capacity; no hidden unlimited queue |
| B15 model_projection | Capture actual model request after waveform result | Blob descriptor/summary present; payload and locator absent |
| B16 identity_not_digest | Publish identical bytes under separate owners/records | No authority sharing inferred from digest equality |
| B17 invalid_parent_output | Commit succeeds provisionally -> parent output fails schema | No blob exported; provisional storage reclaimed |

## F: SPICE and Pi gates

| Case | Real-process trace | Required oracle |
|---|---|---|
| F01 spice_acceptance | Actual ngspice: open netlist -> Circuit -> host discover instantiate -> Session -> host discover transient -> progress -> BlobRef-backed domain waveform -> measure -> release -> reuse | Finite physically plausible measurement against deterministic circuit tolerance; waveform bytes absent from model; released session rejected; no resource-method RPC |
| F02 spice_interrupt | Cancel real running simulator operation | No false successful waveform; owned child cleanup; pin retained until stop |
| F03 pi_regression | Existing Pi suites and unchanged factories with new features disabled/enabled | Existing semantics preserved; pre-existing failures separately identified; no false full-parity claim |

## Gate ordering and reports

A must pass before resource integration is accepted; B before C/D activation; E before F. Independent path-owned development may run in parallel, but acceptance follows A-F. A missing actual ngspice executable blocks F, not A-E. Do not install tools, call paid providers, push, commit, or replace global binaries without the separate authorization required by the owning session.

Report PASS/FAIL/BLOCKED per stable case and SDK, exact child/host source identities, execution count, and retained evidence paths. Performance/large-file tests must distinguish 100 MB/2 GB actual bytes from small limit-boundary fixtures. Passing this matrix does not by itself establish full Pi 1.0.2 parity, target-hardware Doom performance, OS sandboxing, or production release approval.
