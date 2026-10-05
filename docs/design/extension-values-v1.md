---
stage: engineering-design
status: approved-for-implementation
source_baseline: ae7800e265923b2f494806efa40cffa5f713193b
---

# Extension values v1: approved implementation contract

The user approved this architectural direction and subsequently authorized implementation without another approval checkpoint. This document records the bounded contract, not a claim that its implementation or conformance tests already pass.

## Scope and priority

Full stable Pi 1.0.2 extension compatibility is the primary compatibility contract. These additions MUST NOT require annotations in existing Pi extensions, narrow Pi registry/active-tool semantics, or substitute for completing the non-tool Pi API. JavaScript objects, callbacks and synchronous behavior MAY remain local to the Pi adapter; they MUST NOT be presented as ResourceRefs merely because they are objects. Ordinary Pi tools retain ordinary existing behavior. Resource-aware Octet operations opt into additional metadata and lazy projection.

The complete new domain primitives are OperationDescriptor, ResourceRef, BlobRef and Diagnostic. Existing typed JSON values, tool/call, catalog revisions, progress and cancellation are reused. V1 MUST NOT add reliable streams, shared resource access, jobs, remote objects/method invocation, reflection, inheritance, distributed GC, unit/tensor algebra or a second RPC system. ArrayRef, WaveformRef and TableRef are SDK/domain profiles over BlobRef, not kernel concepts. Native resources MUST NOT persist or resurrect.

Implementation order: A SDK normalization; B resources/lifecycle; C descriptors; D applicable-operation lookup/projection; E immutable bulk with local-file.v1; F SPICE demonstration. No features beyond F are authorized by this slice. Unit tests supplement, but MUST NOT replace, real extension subprocess conformance.

## Current system and reuse

* API 0.4 is feature-negotiated JSON-RPC over bounded JSONL; canonical 0.3 is a distinct retained wire. Existing framing, writer, request correlation, cancellation and supervision remain authoritative.
* `crates/octet-agent/src/extension_process/host_requests.rs` owns tool definitions; `validation.rs` validates catalogs/results; `process_api.rs` has both direct and controlled calls; `connection.rs` owns admission/pending execution; `reader.rs` and `protocol_line.rs` own terminal frames.
* `sdk/python/octet_extension/extension.py` supports explicit output schemas. `sdk/rust/src/lib.rs` initially exposes typed input but text-only results. A MUST normalize their basic typed behavior before resource integration.
* Existing media artifacts and composition JSON sidecars are NOT the numerical bulk contract and MUST retain their existing bounds.
* Octet mediates its services, resource admission, bulk grants, tool policy, credentials, deadlines and persistence. Ordinary trusted extension processes are NOT syscall-confined. Capability declarations MUST NOT be described as an OS sandbox.

## Normative language and boundaries

MUST, MUST NOT, SHOULD and MAY are normative requirements. A descriptor is wire data; generated descriptors and Python/Rust author syntax are separate layers. No protocol rule depends on decorators, generic syntax or native memory layout.

All new control methods use the existing JSON-RPC connection, active-parent correlation, frame/queue bounds and cancellation. No request may supply its own authority-bearing session owner. Unknown or unavailable services fail explicitly. Optional API 0.4 feature gates are `resource_refs_v1`, `operation_descriptors_v1`, and `bulk_objects_v1`; resource-bearing operation descriptors require the first two. The first bulk profile is exclusively `local-file.v1`. Old manifests/peers and canonical API 0.3 MUST NOT be implicitly upgraded.

## A. Basic typed values and Diagnostic

The SDKs MUST agree on generated schemas, codecs, missing/default/null behavior, portable JSON integers, finite floating-point scalars, UTF-8 strings, booleans, homogeneous lists, supported nonrecursive records and supported tagged/enum variants. Unsupported type/schema constructs MUST fail registration, not degrade to an unchecked dictionary. Existing lower-level APIs remain available with their existing semantics.

Typed output MUST declare output_schema and emit structured_content conforming to it. It MUST also have an explicit bounded model-facing text projection. Serialization to a text string alone is not typed output. Cancellation and negotiated progress retain their existing semantics; progress is ephemeral and MUST NOT be used for lossless data transport or extend a deadline.

Diagnostic has required `severity` (error/warning/info/hint), `code` (bounded stable domain identifier) and `message` (bounded plain text). Optional fields are `primary`, `related`, `fixes`, and `attachments`. A location identifies a revision-bound workspace source or a BlobRef and a zero-based half-open UTF-8 byte span. Related locations add a message. Fixes contain a title and revision-checked text edits; they are suggestions, never effect authority. Attachments identify existing host-authorized artifacts/blobs, not embedded numerical payloads. Unavailable locations MUST NOT be fabricated.

The existing result envelope carries diagnostic values in the reserved metadata key `octet_diagnostics_v1`; the SDK MUST validate them and include a bounded plain-text diagnostic summary in content. The host MUST validate this reserved profile before retaining it. This introduces no new result envelope or error RPC. Normal structured outputs remain schema-validated; error results may omit structured_content under the existing contract. Source/artifact access and fix application remain independently authorized. V1 diagnostic data MUST NOT activate provisional references from a failed operation.

## B. ResourceRef

Wire shape is a closed object: `{$resource: string, type: string}`. The token is opaque; `type` is a nominal identifier, not an executable class name. Identifiers MUST be host-issued, unpredictable and never reused. Entropy failure MUST refuse issuance. Type identifiers are 1..128 ASCII bytes, starting with a letter and continuing with letters, digits, underscore, dot or hyphen. Tokens are bounded to 128 ASCII bytes. V1 default registry bounds are 256 records per generation and 32 provisional registrations per parent; deployments MAY lower finite bounds, which MUST be exposed in negotiated limits. Retired records with pending cleanup count against the bound until cleanup settles or the process terminates.

### Identity, ownership and type

R1. The host MUST bind each record to the authenticated session owner, extension instance, process generation and nominal type. A client-supplied type or owner MUST NOT establish authority.

R2. A host MUST reject a ResourceRef if owner, extension instance, generation, nominal type or liveness does not match the admitted operation. Cross-extension native-resource transfer is forbidden. Exact type equality is required; no subtyping, inheritance or casts are inferred.

R3. Unknown, fabricated, foreign, stale and released tokens MUST fail before target extension invocation. Foreign/unknown tokens MUST NOT disclose another owner's metadata. An authenticated live reference with an incorrect declared type MAY receive `resource_type_mismatch`; other unavailable references return `resource_unavailable`.

R4. Copies of the same reference do not copy the object or create independent lifetime. The host cannot detect two author-exported objects aliasing the same native allocation; the SDK/author MUST NOT export mutable aliases that evade exclusive admission.

### Registration transaction

R5. `resource/register {parent_request_id,type}` is extension-to-host, available only to a live owned tool/call with negotiated resource support. It returns a PROVISIONAL ResourceRef owned by that exact parent. Registration MUST NOT make the reference discoverable, independently invocable or model-visible as an admitted result. The extension MAY use its native object locally while constructing its result.

R6. Parent result admission is the single host linearization point AFTER complete envelope/content/output-schema/diagnostic/reference validation and BEFORE success publication. It MUST be serialized with parent cancellation, owner retirement and generation retirement. A reader receiving a result frame is not itself successful result admission.

R7. At successful admission, all referenced, declared provisional resource outputs from this parent MUST become ACTIVE atomically. Unexported provisional registrations MUST retire. A reference provisional to another parent MUST fail admission. No subset may activate if any validation fails. Existing active outputs must pass ordinary ownership/type checks.

R8. Failure, invalid output, cancellation winning before admission, timeout or owner/generation retirement MUST retire all provisional resources for the parent. A late success MUST NOT reactivate them. The host MUST NOT project their structured references or a successful creation summary to the model. Arbitrary extension text is not a handle grant; it MUST NOT cause activation or be reparsed to mint one.

R9. If success admission wins before cancellation, exported resources remain active unless independently released/retired. A lost frontend/result delivery MUST NOT cause automatic replay. Native resources are not restored from transcript records.

### Pinning and queued races

R10. All present resource arguments, not merely the receiver, MUST be validated. Distinct referenced resources MUST be acquired exclusively as one atomic admission; duplicate occurrences of one token within a call use one pin. No partial lock acquisition or reentrant bypass is permitted.

R11. Queued calls MUST NOT hold execution pins while waiting for request capacity. After waiting, the host MUST recheck owner, generation, policy, catalog identity and liveness before pinning and starting the serialized frame. Release that wins before admission causes the queued call to fail with zero target invocations. Admission that wins makes release return `resource_busy`.

R12. An execution pin MUST remain held until the corresponding extension execution actually settles or its process generation has terminated. Cancelling/dropping the caller or removing its ordinary pending waiter MUST NOT release the pin. A recognized terminal reply, including a late reply for a tombstoned request, must settle execution bookkeeping. If a frame provably never started, it MAY settle as not executed. If execution termination cannot be established, the host MUST terminate the generation before freeing the pin.

R13. Cancellation/completion races MUST produce one caller disposition and one resource-transaction disposition. Tests MUST deterministically force both orderings. Neither progress nor unrelated child replies settle execution.

### Release and teardown

R14. Release is lifecycle control, not an ordinary resource-receiver method. A host-owned release action and owner-correlated reverse `resource/release {parent_request_id,resource}` use the same registry rule. Release MUST reject a pinned resource, including one pinned by the releasing parent; it MUST NOT wait recursively.

R15. Successful release atomically changes ACTIVE to RETIRED before acknowledgment. New discovery and execution MUST fail from that point. Cleanup has an independent status: pending, completed, failed or unknown. A destructor failure MUST NOT resurrect the reference. An authenticated repeated release MAY report the retained retirement status; a pruned record remains unavailable, never reusable.

R16. The host requests extension-local cleanup with bounded `resource/dispose {resources,reason}` lifecycle control. This is not arbitrary object invocation. The SDK MUST remove retired references from resolution, run disposers on the appropriate local execution lane, and report completed/failed cleanup without restoring validity. A cleanup timeout or transport loss marks cleanup unknown; unresponsive generation cleanup follows bounded process termination. Disposal MUST NOT run while native execution pins remain. Process teardown is a memory/process fence, not proof that an external effect was reversed.

R17. Owner retirement MUST invalidate records independently of UI bindings or subscribed lifecycle hooks. On owner A -> B -> A, retired references MUST NOT revive. Accepted reload, crash/restart and Octet restart invalidate old resources; generation numbering restarting at one MUST NOT restore old identity. Failed candidate negotiation MUST leave a still-active old generation's resources intact.

R18. Retirement may revoke future admission while existing pins drain. Registry bookkeeping MUST remain bounded. Native allocation cleanup and quotas are not OS RSS/CPU/disk quotas.

## C. OperationDescriptor

OperationDescriptor is the existing ToolDefinition plus optional negotiated `operation` metadata: `id`, optional `receiver`, `resource_inputs`, `resource_outputs`. Existing name, description, parameters, output_schema and existing policy/composition fields retain their meanings. Operation ID is a bounded stable nominal identifier; the generated mapping to the existing dispatch name is one-to-one within the extension catalog. Resource input entries contain `{path,type,access:"exclusive"}`; output entries contain `{path,type}`.

O1. Descriptor input/output schemas, resource metadata and handler MUST be one revision-pinned catalog entry. Initialize and dynamic registration MUST validate the complete prospective descriptor before atomic publication. A malformed descriptor MUST NOT partially modify the catalog.

O2. Paths are canonical fixed JSON Pointers into object properties, not wildcards, array indices or reflection. Paths MUST resolve in the declared schema; duplicate/conflicting declarations fail. Missing optional inputs are skipped only when the schema permits absence. Present declared resource inputs MUST be exact ResourceRef values. V1 MUST refuse unsupported resource-containing schema shapes instead of silently losing resource metadata.

O3. `receiver` is ONLY discovery/presentation metadata identifying one declared input path. It MUST NOT alter arguments, dispatch, effects, pins, cancellation, authorization or output semantics. No implicit receiver injection occurs on the wire. Multiple resource arguments are allowed and all are enforced.

O4. Author-level declarations MUST generate schemas/codecs and the canonical resource descriptor together. Python decorators and Rust generics are SDK sugar and MUST NOT be required to implement the wire. Handwritten low-level peers are subject to identical descriptor validation.

O5. All ordinary calls use existing tool/call and existing catalog revisions. There is no operation/invoke or resource/method RPC. The host MUST enforce resource checks on direct ExtensionProcess calls, registered tools and nested composition; a facade-only check is insufficient.

## D. Host applicable-operation lookup and projection

D1. Applicable lookup is host-owned, not an extension-provided discovery tool. It accepts a ResourceRef under an authenticated caller owner. The host MUST first validate the live reference, then select exact matching declared resource input types from the issuing extension instance/generation's catalog snapshot, then apply current policy visibility, then order and bound results. Receiver identifies a preferred matching slot but is not required for a match. Other required inputs remain unsatisfied until supplied.

D2. The protocol-correct baseline order is operation ID ascending, then matched input path ascending. Results identify operation ID, matched input path, primary-receiver flag and catalog revision. Pagination MUST bind its cursor to owner, reference, generation, catalog revision and policy epoch; a changed binding MUST return `catalog_changed`, not silently mix pages. Default limit is 8, maximum 32. Multiple matching slots for one operation MUST be represented explicitly without implying other slots have been supplied.

D3. Semantic/query ranking is optional host product policy applied to the eligible set. It MUST NOT add ineligible candidates. No embedding model, search provider or relevance algorithm is part of the extension protocol or conformance oracle.

D4. Existence, discoverability, model-schema projection and execution authorization are distinct states. Lookup/projection MUST NOT authorize execution, approve an effect, bypass a budget or revive a reference. A policy change after lookup MUST be enforced at execution.

D5. Projection MUST insert exact selected schemas into a bounded next-model-request tool snapshot. In-flight requests retain their original descriptor/handler/catalog revision. Removal/replacement MUST NOT redirect an old call to a new handler. Policy revocation and stale resources still fail closed on old snapshots.

D6. The complete operation catalog MUST NOT automatically enter model context for resource-aware lazy operations. Ordinary Pi tools and Pi registry/active-tool APIs MUST retain their existing semantics. Catalog lookup MUST be bounded and expose no foreign resources or private process internals. The host MUST NOT auto-invoke an operation discovered for a reference.

## E. BlobRef and local-file.v1

BlobRef is a closed value: `{$blob:string, bytes:portable_nonnegative_integer, digest:{algorithm:string,value:string}, media_type:string}`. Its identity is the opaque host-issued `$blob`, NOT the digest. V1 supports integrity algorithm sha256 with lowercase 64-hex encoding; unsupported algorithms fail explicitly. Equal bytes MAY share internal storage but MUST NOT collapse identity, ownership or grants. MIME syntax and length are validated; the kernel MUST NOT need to parse Arrow, tensors or ngspice data.

B1. A BlobRef MUST identify an immutable host-verified byte sequence and authoritative length, media type and integrity metadata. Caller fields MUST match the host record. Knowledge of an ID, digest or locator MUST NOT itself grant access. Unknown/foreign/ungranted references fail before exposing a locator.

B2. bulk/write obtains an active-parent-bound finite-capacity write ticket with a selected transport profile. bulk/commit consumes the ticket, verifies exact declared length and digest against the host-owned snapshot and returns a provisional BlobRef. Duplicate commit MUST NOT create extra blobs or overwrite bytes; unknown/consumed tickets fail explicitly. No partial file is a BlobRef.

B3. Newly committed blobs become externally readable/exported only when the creating parent's validated successful result is admitted. Resource/blob activation shares the parent disposition gate. Failure/cancellation before this point discards unpublished blobs and write tickets. Cancellation after admitted success does not retract committed output. An expected negative scientific result MAY be a successful structured output with diagnostics; an error terminal MUST NOT export new references.

B4. bulk/read validates the caller's owner/grant and metadata, and returns a finite read lease with transport details. A lease pins storage. Read leases and write tickets are bound to owner, extension instance and generation; they do not survive producer/reader process replacement. bulk/release closes a read lease or abandons a write ticket. Callers MUST close their file descriptors/mappings before releasing; already disclosed bytes cannot be made secret again by lease revocation.

B5. Published BlobRefs retained by host result/session ownership MUST survive producer-extension restart, unlike ResourceRefs. Durable retention, when requested by the owning host session, MUST sync bytes and retention metadata before acknowledgment; only durable retained records may survive Octet restart. Recovery MUST reauthorize access and verify retained data before returning a new lease. No locator/lease survives restart. Unretained data is reclaimed after its last result/session retention and read lease end. This is blob retention, not native-object persistence.

B6. The v1 transport capability is exclusively local-file.v1. A transport locator is opaque outside that profile, not domain data, never persisted as part of a domain result, and valid only with its ticket/lease. Resolution MUST remain inside a host-controlled transfer area using private creation and regular/no-follow file handling. Absolute paths, traversal and symlink substitution MUST be refused. The host MUST NOT trust an extension-supplied arbitrary filesystem path as publication authority.

B7. The local-file profile MUST snapshot into undisclosed host-owned backing storage while verifying bytes. Renaming/chmod of a producer's still-writable file alone is insufficient. The immutable committed snapshot MUST NOT change if the producer later changes its scratch file. Future transports MUST preserve BlobRef and domain-wrapper semantics.

B8. The host MUST enforce finite per-object, outstanding-ticket, retained-byte and read-lease limits before admission; counters include reservations and pending publication. Defaults: 256 MiB/object, 512 MiB/owner, 8 write tickets/generation, 32 read leases/generation. Hosts MAY explicitly configure larger finite limits for 100 MB/2 GB workloads; tests MUST NOT silently raise existing media-artifact limits. An unsandboxed producer can physically write outside protocol quotas; commit MUST reject oversize data without unbounded reads/allocations. No OS disk-quota claim is implied.

B9. Wrong length/digest, oversize data, disk-full, interrupted copies and failed durable metadata writes MUST publish nothing, release reservations and clean partial storage. Publication MUST recheck parent/owner/generation after expensive I/O and before atomic admission. Fault injection MUST exercise real file I/O around the failure boundary.

B10. Model requests MUST contain only bounded descriptors/summaries, never numerical payload bytes or transfer locators. mmap and numerical decoding are SDK/domain implementation choices. The host MUST NOT parse arbitrary tool text to recover blob identity.

## Failures, observability and compatibility

New service refusals use existing JSON-RPC error envelopes with bounded stable error data identifying `resource_unavailable`, `resource_type_mismatch`, `resource_busy`, `quota_exceeded`, `catalog_changed`, `blob_unavailable`, `integrity_mismatch`, `size_mismatch`, `storage_unavailable`, or `unsupported_feature` as appropriate. No new error transport exists. Domain diagnostics use Diagnostic, not protocol-error strings or stderr parsing. Foreign/unknown identities share an unavailable response to avoid existence disclosure.

Every admitted call MUST have one caller terminal, independently tracked execution settlement, and one provisional-reference disposition. Trace evidence MUST distinguish admission rejected, cancellation requested, execution settled, reference retired, cleanup completed/failed/unknown, publication committed/abandoned. Trace IDs and bounded counts MUST NOT log secrets, bulk bytes or unrestricted private paths.

Negotiated additions MUST be omitted for old peers; unknown required features or unsupported profiles fail before use. Existing API 0.1/0.2/0.3 contracts and Pi adapter behavior MUST not be silently changed. SDK version, wire version, nominal type version and exact installed-bundle host pin remain separate. New incompatible nominal semantics require a new type ID. No implicit reinterpretation or replay is permitted.

## Acceptance and implementation sequence

A. Normalize Python/Rust typed input/output, cancellation, negotiated progress, diagnostics, generated contracts and common conformance fixtures. Preserve existing lower-level APIs.
B. Implement ResourceRef registry, transactional publication, exclusive pins, independent cleanup facts and real-process lifecycle/race tests.
C. Add generated, validated resource-aware OperationDescriptor to existing catalogs and tool dispatch.
D. Implement host applicable lookup and lazy exact-schema projection; preserve Pi registry semantics.
E. Implement BlobRef, finite grants/retention and local-file.v1 with corruption/cancellation/disk-failure tests.
F. Run an actual ngspice-backed demonstration: open netlist -> Circuit -> discover instantiate -> SimulationSession -> discover transient -> progress -> domain WaveformRef backed by BlobRef -> measure -> release -> rejected reuse. The host must not know ngspice internals and the model must never see waveform bytes.

The accompanying `extension-values-v1-conformance.md` defines mandatory cases and oracles. Missing prerequisites or unimplemented cases are BLOCKED/FAIL, never PASS or silently skipped. No commit, push, global installation, release, paid-provider call or ngspice installation is authorized by this implementation slice.
