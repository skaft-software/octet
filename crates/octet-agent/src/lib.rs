#![cfg_attr(feature = "extensions", deny(missing_docs))]
#![cfg(feature = "extensions")]

//! `octet-agent` — stateful agent loop with tool execution and event streaming.
//!
//! Sits above [`octet_ai`]: the AI crate turns a `Request` into a stream of
//! `StreamEvent`s; this crate orchestrates that stream. It reconstructs
//! provider requests from a persistent JSONL [`Session`], executes tool calls
//! through a small extension boundary, persists every semantic boundary
//! (complete messages and individual tool results — never streaming deltas),
//! and emits [`AgentEvent`]s to the caller. Only `octet-ai`'s public canonical
//! types are used; provider wire formats never leak into this crate.
//!
//! See the [agent design](https://github.com/skaft-software/octet/blob/main/docs/design/octet-agent.md)
//! for the normative design.
//!
//! # Example
//!
//! ```no_run
//! # #[cfg(feature = "agent")]
//! # {
//! use octet_agent::{
//!     Agent, AgentConfig, CoreTools, EffectBroker, ExtensionHost, SandboxConfig, Session,
//! };
//! use octet_ai::{AiClient, CacheRetention, ModelCatalog, ModelId, ReasoningConfig, ReasoningMode};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let catalog = ModelCatalog::builtin()?;
//! let mut extensions = ExtensionHost::new();
//! extensions.load(&CoreTools);
//!
//! let mut agent = Agent::new(AgentConfig {
//!     client: AiClient::new(),
//!     model: catalog.resolve(&ModelId("gpt-4o-mini".into()))?,
//!     session: Session::create("session.jsonl")?,
//!     system: "You are a coding agent.".into(),
//!     sandbox: SandboxConfig::new("."),
//!     effect_broker: EffectBroker::default(),
//!     extensions,
//!     max_turns: Some(40),
//!     reasoning: ReasoningConfig::Off,
//!     reasoning_mode: ReasoningMode::Standard,
//!     cache_retention: CacheRetention::Short,
//!     session_id: None,
//! })?;
//!
//! // Streaming: drive events and control concurrently.
//! let mut run = agent.prompt("Find where auth logic lives").await?;
//! let control = run.control();
//! while let Some(event) = run.next().await {
//!     // Render AgentEvent; use `control` (clonable) to steer/follow_up/abort.
//!     let _ = (&event, &control);
//! }
//! drop(run);
//!
//! // Or run to completion:
//! let output = agent.complete("Fix the failing tests").await?;
//! println!("{}", output.text);
//! # Ok(())
//! # }
//! # }
//! ```
//!
//! # Crash semantics
//!
//! Read-only tools may opt into automatic replay after an unclean crash.
//! Mutating and extension tools are non-replayable by default: an unresolved
//! call is durably paired with an `indeterminate` error and requires explicit
//! user reconciliation, so octet never silently repeats an irreversible effect.
//! Session appends are synced before returning; see [`session`] for the
//! precise persistence and recovery rules.

#[cfg(feature = "agent")]
pub mod agent;
pub mod artifact;
#[cfg(feature = "agent")]
pub mod cache;
#[cfg(feature = "agent")]
pub mod cache_warmer;
#[cfg(feature = "agent")]
pub mod compaction;
#[cfg(feature = "agent")]
pub mod context;
#[cfg(feature = "agent")]
pub mod delegation;
pub mod effect;
#[cfg(feature = "agent")]
pub mod events;
pub mod extension;
pub mod extension_api_policy;
#[rustfmt::skip]
pub mod extension_api_v03;
pub mod extension_api_v04;
pub(crate) mod extension_bulk;
mod extension_bulk_host;
pub mod extension_diagnostics;
pub mod extension_menu;
pub mod extension_message;
pub mod extension_operations;
pub mod extension_policy;
pub mod extension_presentation;
pub mod extension_process;
pub mod extension_provider;
pub mod extension_remote_ui;
#[cfg(feature = "agent")]
pub mod extension_runtime;
pub mod extension_secret;
#[cfg(feature = "agent")]
pub mod goal_driver;
#[cfg(feature = "agent")]
pub mod goal_store;
pub mod input;
pub mod sandbox;
pub mod secure_fs;
#[cfg(feature = "agent")]
pub mod session;
#[cfg(feature = "agent")]
pub mod session_leaf;
pub mod session_types;
#[cfg(feature = "agent")]
mod session_writer;
/// The generic skill substrate containing descriptors, load errors, trust levels, and the registry trait.
#[cfg(feature = "agent")]
pub mod skills;
#[cfg(feature = "agent")]
pub mod telemetry;
pub mod tool;
pub mod tool_composition;
#[allow(missing_docs)]
pub mod tool_inference;
pub mod tools;

#[cfg(feature = "agent")]
pub use agent::{
    public_error_diagnostic, Agent, AgentCompactionMode, AgentConfig, AgentError, CompletionPolicy,
    PreparedHostPrompt, PreparedPromptMode, PreparedSteering, RequestContextEstimate, Run,
    RunControl, RunOutput, SteeringReceipt, TreeNavigationResult,
};
pub use artifact::{
    ArtifactError, ArtifactGenerationSettlement, ArtifactId, ArtifactPublication, ArtifactSource,
    ArtifactStore, ArtifactStoreLimits, PublishedArtifact, ResolvedArtifact,
    DEFAULT_MAX_ARTIFACTS_PER_GENERATION, DEFAULT_MAX_ARTIFACT_BYTES,
    DEFAULT_MAX_ARTIFACT_GENERATION_BYTES, DEFAULT_MAX_INLINE_ARTIFACT_BYTES,
    MAX_ARTIFACT_RELATIVE_PATH_BYTES,
};
#[cfg(feature = "agent")]
pub use cache::{
    analyze_session_cache, analyze_session_cache_stats, CacheMiss, CacheStats,
    CACHE_MISS_NOISE_TOKENS,
};
#[cfg(feature = "agent")]
pub use cache_warmer::{
    CacheWarmMode, CacheWarmingAction, CacheWarmingDecision, CacheWarmingPhase, CacheWarmingState,
    CacheWarmingStatus,
};
#[cfg(feature = "agent")]
pub use compaction::{
    build_branch_handoff_message, build_handoff_message, build_turn_prefix_handoff_message,
    choose_first_kept_by_tokens, finish_branch_handoff, finish_handoff, format_file_operations,
    prepare_branch_handoff, prepare_handoff, serialize_conversation, BranchHandoffPreparation,
    CompactionDetails, HandoffPreparation, BRANCH_SUMMARY_PREAMBLE, DEFAULT_KEEP_RECENT_TOKENS,
    SUMMARIZATION_SYSTEM_PROMPT, SUMMARY_OUTPUT_TOKENS, TURN_PREFIX_OUTPUT_TOKENS,
};
#[cfg(feature = "agent")]
pub use context::{
    ActiveContextCompaction, ContextBreakdown, ContextSnapshot, FinishedContextCompaction,
    RunPhase, RunTerminalState,
};
#[cfg(feature = "agent")]
pub use delegation::{
    delegated_session_reference, delegation_runtime_supports,
    extension_delegated_session_matches_owner, DelegatedAgentStatus, DelegationConfig,
    DelegationError, DelegationLimits, DelegationMode, COLLABORATION_TOOL_NAMES,
};
pub use effect::{
    EffectAuthorization, EffectBroker, EffectBrokerError, EffectGrantToken, EffectIntent,
    EffectPolicy, EffectReceipt, ToolEffect, ToolPolicyDenialCode, EFFECT_POLICY_VERSION,
    MAX_EFFECT_GRANTS, MAX_EFFECT_GRANT_TTL, MAX_EFFECT_INTENT_BYTES,
};
#[cfg(feature = "agent")]
pub use events::{
    AgentEvent, CompactionInfo, CompactionKind, CompactionReason, Control,
    DelegationOrchestrationProvenance, DelegationPolicySource, DelegationTelemetryChild,
    DelegationTelemetrySnapshot, FinishReason, OutputChannel, ProviderOperation, QueueDeliveryMode,
    ToolPolicyDecision,
};
#[cfg(feature = "agent")]
pub use extension::{
    AssistantPersistenceContext, CompactionStrategy, EventObserver, PersistenceMetadataHook,
    PersistenceMetadataProposal, ProviderContextHook, ProviderContextProjection,
    ProviderContextProjectionContext, ProviderContextSessionWait, ProviderRetryAdvice,
    ProviderRetryContext, ProviderRetryHook, ProviderRetryKind,
    MAX_PROVIDER_RETRY_ADDITIONAL_DELAY,
};
pub use extension::{
    Extension, ExtensionHost, PostMutationContext, PostMutationDisposition, PostMutationKind,
    PostMutationRescan, PostMutationState, ToolCallHook, MAX_POST_MUTATION_AFFECTED_RESOURCES,
    MAX_POST_MUTATION_ID_BYTES, MAX_POST_MUTATION_RESOURCE_ID_BYTES,
};
pub use extension_bulk::{BlobDigest, BlobRef, BulkError, BulkLimits};
pub use extension_bulk_host::BulkStorage;
pub use extension_diagnostics::Diagnostic;
pub use extension_menu::{
    ExtensionMenu, ExtensionMenuItem, MAX_EXTENSION_MENU_ARGUMENTS,
    MAX_EXTENSION_MENU_ARGUMENT_BYTES, MAX_EXTENSION_MENU_BYTES, MAX_EXTENSION_MENU_DEPTH,
    MAX_EXTENSION_MENU_DESCRIPTION_BYTES, MAX_EXTENSION_MENU_DETAIL_BYTES,
    MAX_EXTENSION_MENU_ID_BYTES, MAX_EXTENSION_MENU_ITEMS, MAX_EXTENSION_MENU_LABEL_BYTES,
};
pub use extension_policy::{
    ExtensionActionIntent, ExtensionAdapterHints, ExtensionApprovalStore, ExtensionApprovalToken,
    ExtensionIntentPolicy, ExtensionPolicyDecision, ExtensionPolicyError, ExtensionPolicyFrontend,
    MAX_EXTENSION_ACTION_INTENT_BYTES, MAX_EXTENSION_APPROVALS, MAX_EXTENSION_APPROVAL_TTL,
};
pub use extension_presentation::{
    ExtensionPresentationAction, ExtensionPresentationActivity, ExtensionPresentationCollection,
    ExtensionPresentationCollectionKind, ExtensionPresentationDetail, ExtensionPresentationMetrics,
    ExtensionPresentationNode, ExtensionPresentationReference, ExtensionPresentationReferenceKind,
    ExtensionPresentationSnapshot, ExtensionPresentationState, ExtensionPresentationStatus,
    MAX_EXTENSION_PRESENTATION_ACTIONS, MAX_EXTENSION_PRESENTATION_ACTIVITIES,
    MAX_EXTENSION_PRESENTATION_BYTES, MAX_EXTENSION_PRESENTATION_DEPTH,
    MAX_EXTENSION_PRESENTATION_DETAIL_BYTES, MAX_EXTENSION_PRESENTATION_LABEL_BYTES,
    MAX_EXTENSION_PRESENTATION_NODES, MAX_EXTENSION_PRESENTATION_REFERENCES,
};
pub use extension_process::{
    default_extension_roots, discover_extension_manifests, load_extension_manifest_paths,
    AgentSessionEventsRequest, AgentSessionListRequest, AgentSessionMessageRequest,
    AgentSessionSpawnRequest, AgentSessionTargetRequest, AgentSessionWaitRequest,
    CommandDefinition as ExtensionCommandDefinition, CommandOutput as ExtensionCommandOutput,
    ConfirmationRequest as ExtensionConfirmationRequest,
    ConfirmationResponse as ExtensionConfirmationResponse, ContextContribution,
    DiscoveredExtension, ExtensionActivation, ExtensionCapabilities, ExtensionCatalog,
    ExtensionContributions, ExtensionDiagnostic, ExtensionDiagnosticLevel, ExtensionEntrypoint,
    ExtensionEvent, ExtensionExecutionContext, ExtensionFilesystemAccess, ExtensionHealthSnapshot,
    ExtensionHealthState, ExtensionHook, ExtensionHookDisposition, ExtensionHookOutput,
    ExtensionHostState, ExtensionInputRequest, ExtensionInputResponse, ExtensionLifecycleEvent,
    ExtensionLifecycleOutcome, ExtensionManifest, ExtensionManifestInput,
    ExtensionNegotiatedProtocol, ExtensionNotification, ExtensionNotificationLevel,
    ExtensionOperationToken, ExtensionPersistenceMetadata, ExtensionPolicy,
    ExtensionPolicyEvaluationRequest, ExtensionPolicyEvaluationResponse,
    ExtensionPostMutationDisposition, ExtensionProcess, ExtensionProgressEncoding,
    ExtensionProgressEvent, ExtensionProgressStream, ExtensionProtocolLimits,
    ExtensionProtocolRequest, ExtensionProtocolResponse, ExtensionProviderRetryAdvice,
    ExtensionReloadReport, ExtensionRequestId, ExtensionResourceOwner, ExtensionRoot,
    ExtensionRuntimeConfig, ExtensionRuntimeError, ExtensionSource, ExtensionStatusContribution,
    ExtensionTrust, ExtensionUiSurface, OperationDescriptor, RenderedToolCall, ResourceAccess,
    ResourceCleanupStatus, ResourceInput, ResourceOutput, ResourceRef, ResourceReleaseStatus,
    ToolCallOutput as ExtensionToolCallOutput, ToolCatalogUpdateResponse,
    ToolDefinition as ExtensionToolDefinition, ToolRegistrationRequest, ToolRenderSegment,
    DELEGATION_TELEMETRY_SCHEMA, EXTENSION_API_VERSION, EXTENSION_API_VERSION_0_1,
    EXTENSION_API_VERSION_0_2, EXTENSION_API_VERSION_0_3, EXTENSION_API_VERSION_0_4,
    EXTENSION_FEATURE_AGENT_SESSIONS, EXTENSION_FEATURE_AGENT_SESSION_EVENTS_V1,
    EXTENSION_FEATURE_AGENT_SESSION_LIFETIME_V1, EXTENSION_FEATURE_APPROVALS,
    EXTENSION_FEATURE_ARTIFACTS, EXTENSION_FEATURE_COMPACTION_STRATEGY,
    EXTENSION_FEATURE_CONTENT_PARTS, EXTENSION_FEATURE_DELEGATION_TELEMETRY,
    EXTENSION_FEATURE_DYNAMIC_TOOLS, EXTENSION_FEATURE_LIFECYCLE_EVENTS,
    EXTENSION_FEATURE_POLICY_INTENTS, EXTENSION_FEATURE_PROGRESS_DECORATION,
    EXTENSION_FEATURE_REQUEST_CANCELLATION, EXTENSION_FEATURE_REQUEST_PROGRESS,
    EXTENSION_FEATURE_RUNTIME_COMMANDS, EXTENSION_FEATURE_SECRETS, EXTENSION_MANIFEST_FILENAME,
    MAX_EXTENSION_INPUT_PROMPT_BYTES, MAX_EXTENSION_INPUT_VALUE_BYTES,
    MAX_EXTENSION_RESULT_CONTENT_PARTS, MAX_EXTENSION_RESULT_MEDIA_BYTES,
};
pub use extension_provider::{
    ExtensionProviderAuthorizationPolicy, ExtensionProviderAuthorizationStatus,
    ExtensionProviderCatalogEntry, ExtensionProviderOwner, ExtensionProviderRegistry,
    ExtensionProviderRegistryError, ExtensionProviderRoute,
};
pub use extension_remote_ui::{
    validate_remote_ui_line, ExtensionRemoteUiBlockedKind, ExtensionRemoteUiChrome,
    ExtensionRemoteUiChromeRequest, ExtensionRemoteUiCloseRequest, ExtensionRemoteUiCloseResult,
    ExtensionRemoteUiClosed, ExtensionRemoteUiFrame, ExtensionRemoteUiFrameNotification,
    ExtensionRemoteUiKey, ExtensionRemoteUiKeyKind, ExtensionRemoteUiKeyModifier,
    ExtensionRemoteUiMouse, ExtensionRemoteUiMouseButton, ExtensionRemoteUiMouseKind,
    ExtensionRemoteUiOpenRequest, ExtensionRemoteUiOpenResult, ExtensionRemoteUiOperation,
    ExtensionRemoteUiPlacement, ExtensionRemoteUiResize, EXTENSION_FEATURE_REMOTE_UI,
    MAX_EXTENSION_REMOTE_UI_FRAME_BYTES, MAX_EXTENSION_REMOTE_UI_KEY_BYTES,
    MAX_EXTENSION_REMOTE_UI_LINES, MAX_EXTENSION_REMOTE_UI_LINE_BYTES,
    MAX_EXTENSION_REMOTE_UI_REASON_BYTES, MAX_EXTENSION_REMOTE_UI_REVISION,
    MAX_EXTENSION_REMOTE_UI_SGR_BYTES, MAX_EXTENSION_REMOTE_UI_SURFACES,
    MAX_EXTENSION_REMOTE_UI_SURFACE_ID_BYTES, MAX_EXTENSION_REMOTE_UI_TITLE_BYTES,
};
pub use extension_secret::{
    ExtensionSecretBroker, ExtensionSecretError, ExtensionSecretRequest, ExtensionSecretValue,
    MAX_EXTENSION_SECRET_BYTES,
};
#[cfg(feature = "agent")]
pub use goal_driver::{
    continuation_prompt, detect_goal_marker, GoalContinuation, GoalDecision, GoalDriver,
    GoalDriverError, GoalMarker, GoalState, GoalStatus, GoalStore, GoalTurnSource,
    DEFAULT_GOAL_GRACE_PERIOD, GOAL_CONTINUATION_PROMPT_TEMPLATE,
};
#[cfg(feature = "agent")]
pub use goal_store::{
    DurableGoalStore, DurableGoalStoreError, GoalAction, MAX_GOAL_OBJECTIVE_BYTES,
    MAX_GOAL_TURN_BUDGET,
};
pub use input::{InputPart, UserInput};
pub use sandbox::{
    EffectivePolicyValue, EffectiveToolPolicy, PolicyValueSource, SandboxConfig, ShellPathSummary,
    ShellSelection, ToolPolicyProvenance, DEFAULT_MAX_OUTPUT_BYTES,
};
#[cfg(feature = "agent")]
pub use session::{
    compaction_summary_text, CacheWarmRecord, CacheWarmState, Checkpoint, Entry, EntryId,
    EntryMetadata, EntryValue, ExtensionEntryMetadata, ExtensionMetadataProvenance, Session,
    SessionError, SessionRecord, SessionRunOutcome, SessionRunOutcomeStatus, UsageRecord,
    UsageRecordKind, UsageUncertaintyBound, UsageUncertaintyRecord,
    MAX_EXTENSION_ENTRY_METADATA_BYTES, MAX_EXTENSION_ENTRY_METADATA_NAMESPACES,
    MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES, MODEL_VISIBLE_TOOL_IMAGE_LIMIT,
    TOOL_IMAGE_PLACEHOLDER,
};
#[cfg(feature = "agent")]
pub use skills::{
    ContentHash, LoadedSkill, SkillActivationId, SkillDescriptor, SkillId, SkillLoadError,
    SkillQuery, SkillRegistry, SkillSearchResult, SkillSource, SkillTrust,
};
#[cfg(feature = "agent")]
pub use telemetry::{TelemetryObserver, TELEMETRY_SCHEMA};
pub use tool::{
    batch_requests_termination, collect_tool_prompt_contributions, content_hash,
    AdaptivePreviewCoalescer, CancellationToken, ErasedTool, ErasedToolAdapter, OutputStream,
    PartialOutputCheckpointSink, PreviewPublication, ReplaySafety, Tool, ToolConcurrency,
    ToolContext, ToolDefinition, ToolDescriptor, ToolError, ToolInputRequest, ToolInputResponse,
    ToolOutput, ToolOutputContentPart, ToolOutputDetails, ToolOutputMediaKind,
    ToolOutputValidationError, ToolProgress, ToolProgressDecoration, ToolProgressSink,
    ToolPromptContribution, MAX_PROGRESS_CHUNK_BYTES, MAX_TOOL_METADATA_BYTES,
    MAX_TOOL_STRUCTURED_CONTENT_BYTES,
};
#[cfg(feature = "tools")]
pub use tools::{BashTool, CoreTools, EditTool, ReadTool, WriteTool};
