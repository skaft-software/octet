//! Executable extensions discovered from disk and connected over JSON lines.
//!
//! Native [`Extension`]s remain the lowest-overhead option for
//! built-ins. This module adds a language-neutral product boundary: a trusted,
//! explicitly enabled manifest launches one child process and exchanges typed
//! JSON-RPC 2.0 requests, responses, and notifications over stdin/stdout.
//! Capability declarations are consent metadata, not an operating-system
//! sandbox; executable extensions run with the current user's privileges.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
#[cfg(unix)]
use std::os::unix::net::UnixStream as StdUnixStream;
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::ExitStatus;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex as StdMutex, RwLock as StdRwLock, Weak};
use std::time::{Duration, Instant};

use base64::Engine as _;
use octet_ai::{
    AiError, CanonicalStreamAssembler, Diagnostic, HostStreamModel, HostStreamTransport, Media,
    Protocol, ProviderError, ResponseStream, StopReason, StreamEvent, ToolCallId, ToolDef,
    TransportError, TransportPhase, Usage,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, mpsc, oneshot, watch, Mutex, Notify, Semaphore};
#[cfg(windows)]
use windows_sys::Win32::Foundation::HANDLE;
#[cfg(windows)]
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_SET_QUOTA, PROCESS_SUSPEND_RESUME, PROCESS_TERMINATE,
};

use crate::artifact::{ArtifactId, ArtifactPublication, ArtifactSource, ArtifactStore};
use crate::cache_warmer::CacheWarmingAction;
use crate::delegation::{
    ExtensionAgentSessionPolicy, ExtensionDelegationService, ExtensionDelegationSpawnRequest,
};
use crate::effect::{EffectPolicy, ToolEffect};
use crate::events::AgentEvent;
use crate::extension::{
    AssistantPersistenceContext, CacheWarmingDecisionContext, CacheWarmingDecisionHook,
    CompactionStrategy, DynamicToolRegistration, EventObserver, Extension, ExtensionHost,
    PersistenceMetadataHook, PersistenceMetadataProposal, PostMutationContext,
    PostMutationDisposition, ProviderRetryAdvice, ProviderRetryContext, ProviderRetryHook,
    ToolCallHook,
};
use crate::extension_api_v03 as api_v03;
use crate::extension_policy::{
    ExtensionActionIntent, ExtensionApprovalStore, ExtensionApprovalToken, ExtensionPolicyDecision,
};
use crate::extension_presentation::ExtensionPresentationSnapshot;
use crate::extension_provider::{
    ExtensionProviderOwner, ExtensionProviderRegistry, ExtensionProviderRegistryError,
};
use crate::extension_remote_ui::{
    ExtensionRemoteUiChromeRequest, ExtensionRemoteUiCloseRequest, ExtensionRemoteUiClosed,
    ExtensionRemoteUiEditorText, ExtensionRemoteUiFrame, ExtensionRemoteUiFrameNotification,
    ExtensionRemoteUiKey, ExtensionRemoteUiMouse, ExtensionRemoteUiOpenRequest,
    ExtensionRemoteUiOperation, ExtensionRemoteUiResize, RemoteUiChildRequest, RemoteUiMailbox,
    EXTENSION_FEATURE_REMOTE_UI,
};
use crate::extension_secret::{ExtensionSecretBroker, ExtensionSecretRequest};
use crate::tool::{
    CancellationToken, OutputStream, ReplaySafety, Tool, ToolContext, ToolError, ToolOutput,
    ToolOutputContentPart, ToolProgressDecoration, ToolProgressSink,
};
use crate::tool_composition::ToolCompositionConfig;

mod event_bus;
pub use event_bus::ExtensionEventBus;

mod admission;
mod agent_sessions;
mod bulk;
mod composition;
mod connection;
mod contributions;
mod exec;
mod host_requests;
pub use self::exec::ExtensionExecRequest;
mod mcp;
pub use self::mcp::ExtensionMcpRequest;
mod lifecycle_events;
mod manifest;
mod model_control;
mod negotiation;
mod presentation;
mod process_api;
mod terminal_input;
pub use terminal_input::EXTENSION_FEATURE_TERMINAL_INPUT_INTERCEPT;
mod transcript_render;
pub use transcript_render::{
    transcript_private_entry, TranscriptInvalidation, TranscriptRenderContent,
    TranscriptRenderRequest, TranscriptRenderResponse, EXTENSION_FEATURE_TRANSCRIPT_RENDER,
};
mod process_group;
mod process_hooks;
mod process_state;
mod protocol;
mod protocol_line;
mod provider_context;
mod provider_stream;
mod python_runtime;
pub use python_runtime::{provision_python_runtime, PythonRuntimeConfig, PythonRuntimeSetup};
mod reader;
mod resource_paths;
mod session_control;
use self::model_control::dispatch_model_control;
pub use self::model_control::{ExtensionModelControl, PiProviderModelMetadata};
pub use resource_paths::{
    ExtensionDefaultModel, ExtensionResourceDiscoveryReason, ExtensionResourcePaths,
    EXTENSION_FEATURE_RESOURCE_PATHS,
};
use session_control::{dispatch_session_compaction, dispatch_session_control};
pub use session_control::{
    EXTENSION_FEATURE_SESSION_COMPACTION_V1, EXTENSION_FEATURE_SESSION_CONTROL_V1,
};
mod resource_validation;
use bulk::*;
mod resources;
use resource_validation::*;
use resources::*;
pub use resources::{
    OperationDescriptor, ResourceAccess, ResourceCleanupStatus, ResourceInput, ResourceOutput,
    ResourceRef, ResourceReleaseStatus, EXTENSION_FEATURE_OPERATION_DESCRIPTORS_V1,
    EXTENSION_FEATURE_RESOURCE_REFS_V1, MAX_RESOURCE_RECORDS,
    MAX_RESOURCE_REGISTRATIONS_PER_PARENT,
};
mod runtime_config;
pub mod session_leaf;
mod spawn;
pub(crate) use spawn::entrypoint_outside_extension;
mod startup_trace;
pub use self::startup_trace::set_startup_trace_sink;
use self::startup_trace::{
    child_environment as startup_trace_child_environment, forward_child_line,
    process_phase as startup_trace_process_phase,
};
mod tools;
mod validation;

pub use self::admission::validate_extension_flag_value;
use self::admission::*;
use self::agent_sessions::register_agent_session_request;
use self::composition::*;
use self::connection::*;
pub use self::contributions::CommandOutput;
pub use self::contributions::ContextContribution;
pub use self::contributions::ContextPlacement;
pub use self::contributions::ExtensionAutocompleteItem;
pub use self::contributions::ExtensionAutocompleteRegistration;
pub use self::contributions::ExtensionAutocompleteRequest;
pub use self::contributions::ExtensionAutocompleteResponse;
pub use self::contributions::ExtensionCompactionReport;
pub use self::contributions::ExtensionComposerOperation;
pub use self::contributions::ExtensionContextOperation;
pub use self::contributions::ExtensionDialogLifecycle;
pub use self::contributions::ExtensionEditorRequest;
pub use self::contributions::ExtensionEditorResponse;
pub use self::contributions::ExtensionHookDisposition;
pub use self::contributions::ExtensionHookOutput;
pub use self::contributions::ExtensionMessageDelivery;
pub use self::contributions::ExtensionMessageInjection;
pub use self::contributions::ExtensionMessageLifecycle;
pub use self::contributions::ExtensionMessageUpdated;
pub use self::contributions::ExtensionModelOperation;
pub use self::contributions::ExtensionModelSelected;
pub use self::contributions::ExtensionPersistenceMetadata;
pub use self::contributions::ExtensionPostMutationDisposition;
pub use self::contributions::ExtensionProviderRetryAdvice;
pub use self::contributions::ExtensionReasoningSelected;
pub use self::contributions::ExtensionRequestFailure;
pub use self::contributions::ExtensionRequestOutcome;
pub use self::contributions::ExtensionSessionEntryOperation;
pub use self::contributions::ExtensionSessionInfoChanged;
pub use self::contributions::ExtensionShortcutTrigger;
pub use self::contributions::ExtensionStatusContribution;
pub use self::contributions::ExtensionTerminalInput;
pub use self::contributions::ExtensionTerminalOperation;
pub use self::contributions::ExtensionTerminalResize;
pub use self::contributions::ExtensionToolResultReplacement;
pub use self::contributions::ExtensionUiContribution;
pub use self::contributions::ExtensionUserBash;
pub use self::contributions::ExtensionWidgetPlacement;
pub use self::contributions::TerminalGrantLost;
pub use self::host_requests::AgentSessionEventsRequest;
pub use self::host_requests::AgentSessionListRequest;
pub use self::host_requests::AgentSessionMessageRequest;
pub use self::host_requests::AgentSessionModelsRequest;
pub use self::host_requests::AgentSessionPolicy;
pub use self::host_requests::AgentSessionSpawnRequest;
pub use self::host_requests::AgentSessionTargetRequest;
pub use self::host_requests::AgentSessionWaitRequest;
pub use self::host_requests::CommandDefinition;
pub use self::host_requests::ComposerGetRequest;
pub use self::host_requests::ComposerHistoryRequest;
pub use self::host_requests::ComposerTextRequest;
pub use self::host_requests::ComposerTextResult;
pub use self::host_requests::ContextModelCatalogResult;
pub use self::host_requests::ContextPendingMessagesResult;
pub use self::host_requests::ContextSessionManagerResult;
pub use self::host_requests::ContextSkillSummary;
pub use self::host_requests::ContextSnapshotRequest;
pub use self::host_requests::ContextSystemPromptResult;
pub use self::host_requests::ExtensionActiveSkill;
pub use self::host_requests::ExtensionContributions;
pub use self::host_requests::ExtensionEditorCheckpoint;
pub use self::host_requests::ExtensionExecutionContext;
pub use self::host_requests::ExtensionHostState;
pub use self::host_requests::ExtensionModelCost;
pub use self::host_requests::ExtensionModelView;
pub use self::host_requests::ExtensionResourceOwner;
pub use self::host_requests::SessionAppendEntryRequest;
pub use self::host_requests::SessionAppendEntryResult;
pub use self::host_requests::SessionSendMessageRequest;
pub use self::host_requests::SessionSendUserMessageRequest;
pub use self::host_requests::SessionSetLabelRequest;
pub use self::host_requests::SessionSetNameRequest;
pub use self::host_requests::ShortcutDefinition;
pub use self::host_requests::ShortcutRegisterRequest;
pub use self::host_requests::TerminalAcquireRequest;
pub use self::host_requests::TerminalAcquireResult;
pub use self::host_requests::TerminalReleaseRequest;
pub use self::host_requests::TerminalReleaseResult;
pub use self::host_requests::ToolCallOutput;
pub use self::host_requests::ToolCatalogUpdateResponse;
pub use self::host_requests::ToolDefinition;
pub use self::host_requests::ToolRegistrationRequest;
pub use self::host_requests::ToolUnregistrationRequest;
pub use self::host_requests::ToolsSetActiveRequest;
pub use self::host_requests::MAX_EXTENSION_MODEL_API_BYTES;
pub use self::host_requests::MAX_EXTENSION_MODEL_CATALOG_ROWS;
pub use self::host_requests::MAX_EXTENSION_MODEL_FIELD_BYTES;
pub use self::host_requests::MAX_EXTENSION_MODEL_INPUTS;
use self::host_requests::*;
pub use self::lifecycle_events::ExtensionEvent;
pub use self::lifecycle_events::ExtensionOperationToken;
pub use self::lifecycle_events::ExtensionSessionCompactionResult;
pub use self::lifecycle_events::ExtensionSessionLifecycleError;
pub use self::lifecycle_events::ExtensionSessionLifecycleOperation;
pub use self::lifecycle_events::ExtensionSessionLifecycleReceiver;
pub use self::lifecycle_events::ExtensionSessionLifecycleRequest;
pub use self::lifecycle_events::ExtensionSessionLifecycleService;
use self::lifecycle_events::*;
pub use self::manifest::default_extension_roots;
pub use self::manifest::discover_extension_manifests;
pub use self::manifest::load_extension_manifest_paths;
pub use self::manifest::DiscoveredExtension;
pub use self::manifest::ExtensionActivation;
pub use self::manifest::ExtensionCapabilities;
pub use self::manifest::ExtensionCatalog;
pub use self::manifest::ExtensionDiagnostic;
pub use self::manifest::ExtensionDiagnosticLevel;
pub use self::manifest::ExtensionEntrypoint;
pub use self::manifest::ExtensionFilesystemAccess;
pub use self::manifest::ExtensionFlag;
pub use self::manifest::ExtensionFlagType;
pub use self::manifest::ExtensionHook;
pub use self::manifest::ExtensionLifecycleProfile;
pub use self::manifest::ExtensionManifest;
pub use self::manifest::ExtensionManifestInput;
pub use self::manifest::ExtensionPolicy;
pub use self::manifest::ExtensionRoot;
pub use self::manifest::ExtensionRuntimeSettings;
pub use self::manifest::ExtensionRuntimeSharing;
pub use self::manifest::ExtensionSource;
pub use self::manifest::ExtensionStartDecision;
pub use self::manifest::ExtensionTrust;
pub use self::manifest::ExtensionUiSurface;
pub use self::manifest::ManifestContributions;
use self::negotiation::*;
use self::presentation::*;
pub use self::process_group::begin_host_shutdown;
pub use self::process_group::force_kill_registered_process_groups;
#[cfg(all(test, unix))]
pub(crate) use self::process_group::process_group_registered_for_test;
#[cfg(all(test, unix))]
pub(crate) use self::process_group::process_is_live_for_test;
pub use self::process_group::sanitized_subprocess_environment;
pub use self::process_group::terminate_bash_process_groups;
#[cfg(unix)]
pub use self::process_group::wait_for_bash_process;
#[cfg(unix)]
pub use self::process_group::BashProcessHandoff;
#[cfg(unix)]
pub use self::process_group::BashProcessLaunch;
pub use self::process_group::ProcessGroupGuard;
#[cfg(windows)]
pub use self::process_group::WindowsProcessLaunch;
pub use self::process_group::DEFAULT_EXTENSION_MANIFEST_BYTES;
pub use self::process_group::DEFAULT_EXTENSION_MESSAGE_BYTES;
pub use self::process_group::DELEGATION_TELEMETRY_SCHEMA;
pub use self::process_group::EXTENSION_API_VERSION;
pub use self::process_group::EXTENSION_API_VERSION_0_1;
pub use self::process_group::EXTENSION_API_VERSION_0_2;
pub use self::process_group::EXTENSION_API_VERSION_0_3;
pub use self::process_group::EXTENSION_API_VERSION_0_4;
pub use self::process_group::EXTENSION_FEATURE_ACTIVE_TOOLS;
pub use self::process_group::EXTENSION_FEATURE_AGENT_MODEL_SELECTION_V1;
pub use self::process_group::EXTENSION_FEATURE_AGENT_SESSIONS;
pub use self::process_group::EXTENSION_FEATURE_AGENT_SESSION_EVENTS_V1;
pub use self::process_group::EXTENSION_FEATURE_AGENT_SESSION_LIFETIME_V1;
pub use self::process_group::EXTENSION_FEATURE_APPROVALS;
pub use self::process_group::EXTENSION_FEATURE_ARTIFACTS;
pub use self::process_group::EXTENSION_FEATURE_AUTOCOMPLETE;
pub use self::process_group::EXTENSION_FEATURE_AUTOCOMPLETE_EDIT_V1;
pub use self::process_group::EXTENSION_FEATURE_BEFORE_PROMPT_STATE_V1;
pub use self::process_group::EXTENSION_FEATURE_CACHE_WARMING_DECISION;
pub use self::process_group::EXTENSION_FEATURE_COMPACTION_STRATEGY;
pub use self::process_group::EXTENSION_FEATURE_COMPOSER;
pub use self::process_group::EXTENSION_FEATURE_CONTENT_PARTS;
pub use self::process_group::EXTENSION_FEATURE_DELEGATION_TELEMETRY;
pub use self::process_group::EXTENSION_FEATURE_DYNAMIC_TOOLS;
pub use self::process_group::EXTENSION_FEATURE_DYNAMIC_TOOL_RENDERERS;
pub use self::process_group::EXTENSION_FEATURE_EDITOR_HANDOFF;
pub use self::process_group::EXTENSION_FEATURE_LIFECYCLE_EVENTS;
pub use self::process_group::EXTENSION_FEATURE_LIFECYCLE_EVENTS_V2;
pub use self::process_group::EXTENSION_FEATURE_MESSAGE_INJECTION;
pub use self::process_group::EXTENSION_FEATURE_MODEL_CATALOG;
pub use self::process_group::EXTENSION_FEATURE_PIPELINE_HOOKS_V1;
pub use self::process_group::EXTENSION_FEATURE_POLICY_INTENTS;
pub use self::process_group::EXTENSION_FEATURE_PROGRESS_DECORATION;
pub use self::process_group::EXTENSION_FEATURE_PROVIDER_CREDENTIALS;
pub use self::process_group::EXTENSION_FEATURE_REQUEST_CANCELLATION;
pub use self::process_group::EXTENSION_FEATURE_REQUEST_PROGRESS;
pub use self::process_group::EXTENSION_FEATURE_RUNTIME_COMMANDS;
pub use self::process_group::EXTENSION_FEATURE_SECRETS;
pub use self::process_group::EXTENSION_FEATURE_SEMANTIC_UI;
pub use self::process_group::EXTENSION_FEATURE_SESSION_CONTEXT;
pub use self::process_group::EXTENSION_FEATURE_SESSION_ENTRIES;
pub use self::process_group::EXTENSION_FEATURE_SHORTCUTS;
pub use self::process_group::EXTENSION_FEATURE_SYSTEM_PROMPT_READ;
pub use self::process_group::EXTENSION_FEATURE_TERMINAL_HANDOFF;
pub use self::process_group::EXTENSION_FEATURE_TERMINAL_INPUT;
pub use self::process_group::EXTENSION_FEATURE_TOOL_COMPOSITION;
pub use self::process_group::EXTENSION_MANIFEST_FILENAME;
pub use self::process_group::MAX_EXTENSION_AUTOCOMPLETE_ITEMS;
pub use self::process_group::MAX_EXTENSION_AUTOCOMPLETE_TEXT_BYTES;
pub use self::process_group::MAX_EXTENSION_BASH_COMMAND_BYTES;
pub use self::process_group::MAX_EXTENSION_CHILD_REQUEST_IDS_PER_GENERATION;
pub use self::process_group::MAX_EXTENSION_COMPOSER_TEXT_BYTES;
pub use self::process_group::MAX_EXTENSION_CONTEXT_ACTIVE_SKILLS;
pub use self::process_group::MAX_EXTENSION_CONTEXT_LABEL_BYTES;
pub use self::process_group::MAX_EXTENSION_CONTEXT_PATH_BYTES;
pub use self::process_group::MAX_EXTENSION_CONTEXT_SKILL_FIELD_BYTES;
pub use self::process_group::MAX_EXTENSION_EDITOR_TEXT_BYTES;
pub use self::process_group::MAX_EXTENSION_FLAGS;
pub use self::process_group::MAX_EXTENSION_INJECTED_MESSAGE_BYTES;
pub use self::process_group::MAX_EXTENSION_INPUT_PROMPT_BYTES;
pub use self::process_group::MAX_EXTENSION_INPUT_VALUE_BYTES;
pub use self::process_group::MAX_EXTENSION_MESSAGE_UPDATED_TEXT_BYTES;
pub use self::process_group::MAX_EXTENSION_REQUEST_ERROR_DETAIL_BYTES;
pub use self::process_group::MAX_EXTENSION_RESULT_CONTENT_PARTS;
pub use self::process_group::MAX_EXTENSION_RESULT_MEDIA_BYTES;
pub use self::process_group::MAX_EXTENSION_SESSION_ENTRY_DATA_BYTES;
pub use self::process_group::MAX_EXTENSION_SESSION_ENTRY_TYPE_BYTES;
pub use self::process_group::MAX_EXTENSION_SESSION_LABEL_BYTES;
pub use self::process_group::MAX_EXTENSION_SESSION_LIFECYCLE_QUEUE;
pub use self::process_group::MAX_EXTENSION_SESSION_NAME_BYTES;
pub use self::process_group::MAX_EXTENSION_SHORTCUTS;
pub use self::process_group::MAX_EXTENSION_SHORTCUT_DESCRIPTION_BYTES;
pub use self::process_group::MAX_EXTENSION_SHORTCUT_ID_BYTES;
pub use self::process_group::MAX_EXTENSION_SHORTCUT_KEY_BYTES;
pub use self::process_group::MAX_EXTENSION_SYSTEM_PROMPT_BYTES;
pub use self::process_group::MAX_EXTENSION_TERMINAL_GRANT_ID_BYTES;
pub use self::process_group::MAX_EXTENSION_TERMINAL_INPUT_BYTES;
pub use self::process_group::MAX_EXTENSION_UI_ENTRIES;
pub use self::process_group::MAX_EXTENSION_UI_INDICATOR_FRAMES;
pub use self::process_group::MAX_EXTENSION_UI_KEY_BYTES;
pub use self::process_group::MAX_EXTENSION_UI_LINES;
pub use self::process_group::MAX_EXTENSION_UI_TEXT_BYTES;
use self::process_group::*;
use self::process_hooks::*;
pub use self::process_state::CommandRequest;
pub use self::process_state::ContextRequest;
pub use self::process_state::ExtensionIdentity;
pub use self::process_state::ExtensionLifecycleTurnContext;
pub use self::process_state::ExtensionProcess;
pub use self::process_state::HookRequest;
pub use self::process_state::InitializeRequest;
pub use self::process_state::InitializeResponse;
pub use self::process_state::MenuRequest;
pub use self::process_state::ShortcutRequest;
pub use self::process_state::StatusRequest;
pub use self::process_state::ToolCallRequest;
pub use self::process_state::ToolRenderRequest;
use self::process_state::*;
pub use self::protocol::ConfirmationRequest;
pub use self::protocol::ConfirmationResponse;
pub use self::protocol::ExtensionHealthSnapshot;
pub use self::protocol::ExtensionHealthState;
pub use self::protocol::ExtensionInputRequest;
pub use self::protocol::ExtensionInputResponse;
pub use self::protocol::ExtensionLifecycleEvent;
pub use self::protocol::ExtensionLifecycleOutcome;
pub use self::protocol::ExtensionNegotiatedProtocol;
pub use self::protocol::ExtensionNotification;
pub use self::protocol::ExtensionNotificationLevel;
pub use self::protocol::ExtensionPolicyEvaluationRequest;
pub use self::protocol::ExtensionPolicyEvaluationResponse;
pub use self::protocol::ExtensionProgressEncoding;
pub use self::protocol::ExtensionProgressEvent;
pub use self::protocol::ExtensionProgressStream;
pub use self::protocol::ExtensionProtocolLimits;
pub use self::protocol::ExtensionProtocolRequest;
pub use self::protocol::ExtensionProtocolResponse;
pub use self::protocol::ExtensionRequestId;
pub use self::protocol::RenderedToolCall;
pub use self::protocol::ResourceProtocolLimits;
pub use self::protocol::ToolRenderSegment;
pub use self::protocol::EXTENSION_FEATURE_BUILTIN_TOOL_OVERRIDES;
pub use self::protocol::EXTENSION_FEATURE_TOOL_PROMPT_METADATA;
use self::protocol::*;
use self::protocol_line::*;
use self::provider_stream::*;
use self::reader::*;
pub use self::runtime_config::ExtensionReloadReport;
pub use self::runtime_config::ExtensionRuntimeConfig;
pub use self::runtime_config::ExtensionRuntimeError;
use self::runtime_config::*;
use self::spawn::*;
use self::tools::*;
use self::validation::*;

#[cfg(windows)]
#[link(name = "ntdll")]
#[allow(non_snake_case)]
unsafe extern "system" {
    fn NtResumeProcess(process_handle: HANDLE) -> i32;
}

/// Stable JSON-RPC method names for executable-extension SDKs.
pub mod methods {
    /// Host-to-extension initialization handshake.
    pub const INITIALIZE: &str = "initialize";
    /// Host-to-extension tool invocation.
    pub const TOOL_CALL: &str = "tool/call";
    /// Host-to-extension slash-command invocation.
    pub const COMMAND_EXECUTE: &str = "command/execute";
    /// Host-to-extension terminal shortcut invocation.
    pub const SHORTCUT_EXECUTE: &str = "shortcut/execute";
    /// Host-to-extension lifecycle hook invocation.
    pub const HOOK_RUN: &str = "hook/run";
    /// Host request for prompt context.
    pub const CONTEXT_COLLECT: &str = "context/collect";
    /// Host request for a semantic status/header/footer contribution.
    pub const STATUS_COLLECT: &str = "status/collect";
    /// Host request for the extension's `/extensions` options menu.
    pub const MENU_COLLECT: &str = "menu/collect";
    /// Host request for semantic tool-renderer output.
    pub const TOOL_RENDER: &str = "tool/render";
    /// Graceful lifecycle shutdown request.
    pub const SHUTDOWN: &str = "shutdown";
    /// Idempotent cancellation of a host or extension-originated request.
    pub const CANCEL_REQUEST: &str = "$/cancelRequest";
    /// Request-scoped ephemeral progress.
    pub const PROGRESS: &str = "$/progress";
    /// Extension request to compact the idle active-host session (API 0.4).
    pub const SESSION_COMPACT: &str = "session/compact";
    /// Extension request to create a durable active-host session.
    pub const SESSION_CREATE: &str = "session/create";
    /// Extension request to fork the active host session.
    pub const SESSION_FORK: &str = "session/fork";
    /// Extension request to reopen the active host session from disk.
    pub const SESSION_RELOAD: &str = "session/reload";
    /// Extension request to switch the active host session.
    pub const SESSION_SWITCH: &str = "session/switch";
    /// Extension-to-host user notification.
    pub const NOTIFICATION: &str = "notification";
    /// Extension-to-host interactive confirmation request.
    pub const CONFIRMATION_REQUEST: &str = "confirmation/request";
    /// Extension-to-host unsolicited prompt context.
    pub const CONTEXT_CONTRIBUTION: &str = "context/contribution";
    /// Extension-to-host unsolicited semantic UI contribution.
    pub const STATUS_CONTRIBUTION: &str = "status/contribution";
    /// Extension-to-host bounded semantic UI snapshot.
    pub const UI_CONTRIBUTION: &str = "ui/contribution";
    /// Extension-to-host host-owned editor request.
    pub const UI_EDITOR: &str = "ui/editor";
    /// Host-to-extension editor snapshot notification.
    pub const UI_EDITOR_STATE: &str = "ui/editor-state";
    /// Host-to-extension observer-only normalized terminal input.
    pub const UI_TERMINAL_INPUT: &str = "ui/terminal-input";
    /// Host-to-extension observer-only terminal resize.
    pub const UI_RESIZE: &str = "ui/resize";
    /// Extension request to open a cached host-rendered surface.
    pub const UI_OPEN: &str = "ui/open";
    /// Extension request to close its cached surface.
    pub const UI_CLOSE: &str = "ui/close";
    /// Extension-to-host complete cached line snapshot notification.
    pub const UI_FRAME: &str = "ui/frame";
    /// Host-to-extension focused normalized key notification.
    pub const UI_KEY: &str = "ui/key";
    /// Host-to-extension composer-slot text replacement for a mounted editor.
    pub const UI_EDITOR_TEXT: &str = "ui/editor-text";
    /// Host-to-extension normalized fullscreen mouse notification.
    pub const UI_MOUSE: &str = "ui/mouse";
    /// Host-to-extension surface closure notification.
    pub const UI_CLOSED: &str = "ui/closed";
    /// Owner-fenced host-state replacement for retained API 0.4 UI contexts.
    pub const CONTEXT_UPDATED: &str = "context/updated";
    /// Extension-to-host autocomplete registration request.
    pub const AUTOCOMPLETE_REGISTER: &str = "ui/autocomplete/register";
    /// Host-to-extension bounded autocomplete query.
    pub const AUTOCOMPLETE_COMPLETE: &str = "ui/autocomplete/complete";
    /// Extension-to-host complete frontend-neutral presentation snapshot.
    pub const PRESENTATION_UPDATE: &str = "presentation/update";
    /// Extension-to-host structured policy intent.
    pub const POLICY_EVALUATE: &str = "policy/evaluate";
    /// Extension-to-host ephemeral input request.
    pub const INPUT_REQUEST: &str = "input/request";
    /// Extension-to-host bounded artifact ingestion request.
    pub const ARTIFACT_PUBLISH: &str = "artifact/publish";
    /// Extension-to-host owner-scoped secret lookup.
    pub const SECRET_GET: &str = "secret/get";
    /// Extension-to-host live tool registration request.
    pub const TOOLS_REGISTER: &str = "tools/register";
    /// Extension-to-host live tool removal request.
    pub const TOOLS_UNREGISTER: &str = "tools/unregister";
    /// Extension request for a model-tool parent's composition context.
    pub const COMPOSITION_CONTEXT: &str = "composition/context";
    /// Extension request to dispatch a nested call through the host tool boundary.
    pub const COMPOSITION_CALL: &str = "composition/call";
    /// Extension request to update the parent-scoped composition store.
    pub const COMPOSITION_STORE: &str = "composition/store";
    /// Extension-to-host completion of an initial provider catalog batch.
    pub const PROVIDERS_COMPLETE: &str = "providers/complete";
    /// Extension-to-host atomic provider catalog registration.
    pub const PROVIDERS_REGISTER: &str = "providers/register";
    /// Extension-to-host atomic provider catalog replacement.
    pub const PROVIDERS_UPDATE: &str = "providers/update";
    /// Extension-to-host provider catalog removal.
    pub const PROVIDERS_UNREGISTER: &str = "providers/unregister";
    /// Host-to-extension provider inference stream request.
    pub const PROVIDER_STREAM: &str = "provider/stream";
    /// Extension-to-host ordered provider stream event notification.
    pub const PROVIDER_EVENT: &str = "provider/event";
    /// Host-to-extension best-effort provider stream cancellation.
    pub const PROVIDER_CANCEL: &str = "provider/cancel";
    /// Extension-to-host explicit host-policy authorization request.
    pub const PROVIDER_AUTH_REQUEST: &str = "provider/auth/request";
    /// Extension-to-host explicit authorization revocation request.
    pub const PROVIDER_AUTH_REVOKE: &str = "provider/auth/revoke";
    /// Extension request to create one host-owned child model session.
    pub const AGENT_SPAWN: &str = "agent/spawn";
    /// Extension request to send steering input to an owned child session.
    pub const AGENT_MESSAGE: &str = "agent/message";
    /// Extension request to queue a follow-up task on an owned child session.
    pub const AGENT_FOLLOW_UP: &str = "agent/follow_up";
    /// Extension request to inspect owned child sessions.
    pub const AGENT_LIST: &str = "agent/list";
    /// Discover configured worker model choices for an owner.
    pub const AGENT_MODELS: &str = "agent/models";
    /// Extension request to wait for owned child-session state changes.
    pub const AGENT_WAIT: &str = "agent/wait";
    /// Extension request to interrupt an owned child-session tree.
    pub const AGENT_INTERRUPT: &str = "agent/interrupt";
    /// API 0.4 loss-detecting observation of an owned child session.
    pub const AGENT_EVENTS: &str = "agent/events";
    /// API 0.4 shutdown of one owned child-session tree (not settlement).
    pub const AGENT_STOP: &str = "agent/stop";
    /// Observational session start.
    pub const SESSION_STARTED: &str = "session/started";
    /// Observational session terminal boundary.
    pub const SESSION_SETTLED: &str = "session/settled";
    /// Observational turn start.
    pub const TURN_STARTED: &str = "turn/started";
    /// Observational turn terminal boundary.
    pub const TURN_SETTLED: &str = "turn/settled";
    /// Observational global tool start.
    pub const TOOL_STARTED: &str = "tool/started";
    /// Observational global tool terminal boundary.
    pub const TOOL_SETTLED: &str = "tool/settled";
    /// Extension request for the current host composer snapshot.
    pub const COMPOSER_GET: &str = "composer/get";
    /// Extension request to replace the complete host composer text.
    pub const COMPOSER_SET: &str = "composer/set";
    /// Extension request to insert text at the host composer cursor.
    pub const COMPOSER_INSERT: &str = "composer/insert";
    /// Extension request to seed older native prompt history.
    pub const COMPOSER_HISTORY: &str = "composer/history";
    /// Extension request to register one runtime terminal shortcut.
    pub const SHORTCUT_REGISTER: &str = "shortcut/register";
    /// Extension request to append one extension-owned durable session entry.
    pub const SESSION_APPEND_ENTRY: &str = "session/append_entry";
    /// Extension request to set the active host session name.
    pub const SESSION_SET_NAME: &str = "session/set_name";
    /// Extension request to label one durable session entry.
    pub const SESSION_SET_LABEL: &str = "session/set_label";
    /// Extension request to inject one bounded assistant or system message.
    pub const SESSION_SEND_MESSAGE: &str = "session/send_message";
    /// Extension request to inject one bounded user message.
    pub const SESSION_SEND_USER_MESSAGE: &str = "session/send_user_message";
    /// Extension request to replace the active host tool set.
    pub const TOOLS_SET_ACTIVE: &str = "tools/set_active";
    /// Host-to-extension runtime shortcut activation.
    pub const SHORTCUT_TRIGGER: &str = "shortcut/trigger";
    /// Host-to-extension assistant message start.
    pub const MESSAGE_STARTED: &str = "message/started";
    /// Host-to-extension coalesced assistant message deltas.
    pub const MESSAGE_UPDATED: &str = "message/updated";
    /// Host-to-extension assistant message terminal boundary.
    pub const MESSAGE_SETTLED: &str = "message/settled";
    /// Host-to-extension compaction start.
    pub const COMPACTION_STARTED: &str = "compaction/started";
    /// Host-to-extension compaction success.
    pub const COMPACTION_SETTLED: &str = "compaction/settled";
    /// Host-to-extension compaction failure.
    pub const COMPACTION_FAILED: &str = "compaction/failed";
    /// Host-to-extension session name or label change.
    pub const SESSION_INFO_CHANGED: &str = "session/info_changed";
    /// Host-to-extension dialog start.
    pub const DIALOG_STARTED: &str = "dialog/started";
    /// Host-to-extension dialog terminal boundary.
    pub const DIALOG_SETTLED: &str = "dialog/settled";
    /// Host-to-extension model selection change.
    pub const MODEL_SELECTED: &str = "model/selected";
    /// Host-to-extension reasoning selection change.
    pub const REASONING_SELECTED: &str = "reasoning/selected";
    /// Host-to-extension user `!`/`!!` bash execution.
    pub const BASH_USER: &str = "bash/user";
    /// Extension request to cede the foreground raw terminal.
    pub const TERMINAL_ACQUIRE: &str = "terminal/acquire";
    /// Extension request to return a ceded foreground terminal.
    pub const TERMINAL_RELEASE: &str = "terminal/release";
    /// Host-to-extension revocation of an outstanding terminal grant.
    pub const TERMINAL_GRANT_LOST: &str = "terminal/grant-lost";
    /// Extension request for the active session-manager snapshot.
    pub const CONTEXT_SESSION_MANAGER: &str = "context/session_manager";
    /// Extension request for the bounded pending-message count.
    pub const CONTEXT_PENDING_MESSAGES: &str = "context/pending_messages";
    /// Extension request for the host-owned composed system prompt text.
    pub const CONTEXT_SYSTEM_PROMPT: &str = "context/system_prompt";
    /// Extension request for a provider/model-scoped credential resolution.
    pub const PROVIDER_CREDENTIALS: &str = "provider/credentials";
    /// Extension request for the selected model view.
    pub const CONTEXT_MODEL: &str = "context/model";
    /// Extension request for the secret-free model catalog.
    pub const CONTEXT_MODEL_CATALOG: &str = "context/model_catalog";
}

macro_rules! impl_owner_scoped_host_request {
    ($type:ty) => {
        impl OwnerScopedHostRequest for $type {
            fn parent_request_id(&self) -> u64 {
                self.parent_request_id
            }

            fn request_resource_owner(&self) -> Option<&ExtensionResourceOwner> {
                self.resource_owner.as_ref()
            }
        }
    };
}

impl_owner_scoped_host_request!(ComposerGetRequest);
impl_owner_scoped_host_request!(ComposerHistoryRequest);
impl_owner_scoped_host_request!(ComposerTextRequest);
impl_owner_scoped_host_request!(ShortcutRegisterRequest);
impl_owner_scoped_host_request!(SessionAppendEntryRequest);
impl_owner_scoped_host_request!(SessionSetNameRequest);
impl_owner_scoped_host_request!(SessionSetLabelRequest);
impl_owner_scoped_host_request!(SessionSendMessageRequest);
impl_owner_scoped_host_request!(SessionSendUserMessageRequest);
impl_owner_scoped_host_request!(ToolsSetActiveRequest);
impl_owner_scoped_host_request!(TerminalAcquireRequest);
impl_owner_scoped_host_request!(TerminalReleaseRequest);
impl_owner_scoped_host_request!(ContextSnapshotRequest);
impl_owner_scoped_host_request!(ExtensionExecRequest);
impl_owner_scoped_host_request!(ExtensionMcpRequest);
impl_owner_scoped_host_request!(ExtensionRemoteUiOpenRequest);
impl_owner_scoped_host_request!(ExtensionRemoteUiCloseRequest);
impl_owner_scoped_host_request!(ExtensionRemoteUiChromeRequest);

#[cfg(test)]
mod tests;
#[cfg(test)]
mod ui_transport_tests;

#[cfg(all(test, unix))]
mod remote_ui_tests;
#[cfg(all(test, unix))]
mod resources_tests;
