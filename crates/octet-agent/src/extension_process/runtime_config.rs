//! Runtime configuration, offered host services and runtime errors.

use super::*;

/// Runtime knobs for one executable extension process.
#[derive(Clone)]
pub struct ExtensionRuntimeConfig {
    /// Workspace used as the child working directory and execution context.
    pub workspace: PathBuf,
    /// Initial session/model/skill state.
    pub host_state: ExtensionHostState,
    /// Manifest-declared CLI values resolved by the product before startup.
    pub flag_values: BTreeMap<String, serde_json::Value>,
    /// Offer the optional host-owned child model-session service. The product
    /// must bind an enabled delegation runtime before the service is usable.
    pub agent_sessions: bool,
    /// Offer API `0.4` request-scoped tool composition. A model-tool parent
    /// must separately supply the host dispatcher through its progress sink.
    pub tool_composition: bool,
    /// Shared host-owned immutable bulk storage. Only configured API 0.4 peers
    /// may negotiate bulk_objects_v1; this does not change media artifact limits.
    pub bulk_store: Option<crate::BulkStorage>,
    /// Optional bounded API 0.3 active-session lifecycle driver. It is offered
    /// only when configured; it remains inactive until the product binds a safe
    /// interactive idle boundary. Legacy processes never retain this service.
    pub session_lifecycle: Option<ExtensionSessionLifecycleService>,
    /// Optional session-isolated data bus; never bind to workspace-shared processes.
    pub event_bus: Option<Arc<ExtensionEventBus>>,
    /// Optional frontend wake/consumer binding for cached API 0.4 remote UI.
    /// Without this binding the host never offers or admits `remote_ui`.
    pub remote_ui: Option<Arc<Notify>>,
    /// Offer single-use approval redemption. A trusted frontend can issue a
    /// capability with [`ExtensionProcess::respond_to_policy_approval`].
    pub approvals: bool,
    /// Optional owner-scoped secret provider. The `secrets` feature is offered
    /// only when this is configured and the manifest declares secret names.
    /// The broker must not strongly retain this extension process.
    pub secret_broker: Option<Arc<dyn ExtensionSecretBroker>>,
    /// Shared lifecycle registry for API 0.3 extension provider catalogs.
    /// When absent, provider capabilities may not be negotiated or published.
    pub provider_registry: Option<Arc<ExtensionProviderRegistry>>,
    /// Bounded number of provider events held for one caller before the host
    /// cancels the stream rather than buffering unbounded output.
    pub provider_stream_buffer: usize,
    /// Maximum quiet interval between accepted provider stream events.
    pub provider_stream_idle_timeout: Duration,
    /// Absolute maximum duration of an accepted provider stream.
    pub provider_stream_deadline: Duration,
    /// Maximum duration of one request.
    pub request_timeout: Duration,
    /// Per-stage shutdown timeout, applied once to the shutdown request/ack and
    /// again while waiting for the child to exit during shutdown or reload.
    pub shutdown_timeout: Duration,
    /// Maximum serialized JSON line size.
    pub max_message_bytes: usize,
    /// Maximum concurrent requests to one extension.
    pub max_pending_requests: usize,
    /// Maximum complete frames awaiting the dedicated serialized writer.
    pub writer_queue_capacity: usize,
    /// Grace allowed after cooperative cancellation before a non-responsive
    /// extension generation is force-terminated.
    pub cancellation_grace: Duration,
    /// Retention window for cancelled request IDs and late-reply diagnosis.
    pub tombstone_ttl: Duration,
    /// Whether this process owns its legacy per-process restart supervisor.
    /// A host-level runtime manager disables this and provides one durable
    /// supervisor for the governed fleet instead.
    pub supervise: bool,
}

impl std::fmt::Debug for ExtensionRuntimeConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExtensionRuntimeConfig")
            .field("workspace", &self.workspace)
            .field("host_state", &self.host_state)
            .field(
                "flag_value_names",
                &self.flag_values.keys().collect::<Vec<_>>(),
            )
            .field("agent_sessions", &self.agent_sessions)
            .field("tool_composition", &self.tool_composition)
            .field("bulk_store_configured", &self.bulk_store.is_some())
            .field(
                "session_lifecycle_configured",
                &self.session_lifecycle.is_some(),
            )
            .field("event_bus_configured", &self.event_bus.is_some())
            .field("remote_ui_configured", &self.remote_ui.is_some())
            .field("approvals", &self.approvals)
            .field("secret_broker_configured", &self.secret_broker.is_some())
            .field(
                "provider_registry_configured",
                &self.provider_registry.is_some(),
            )
            .field("provider_stream_buffer", &self.provider_stream_buffer)
            .field(
                "provider_stream_idle_timeout",
                &self.provider_stream_idle_timeout,
            )
            .field("provider_stream_deadline", &self.provider_stream_deadline)
            .field("request_timeout", &self.request_timeout)
            .field("shutdown_timeout", &self.shutdown_timeout)
            .field("max_message_bytes", &self.max_message_bytes)
            .field("max_pending_requests", &self.max_pending_requests)
            .field("writer_queue_capacity", &self.writer_queue_capacity)
            .field("cancellation_grace", &self.cancellation_grace)
            .field("tombstone_ttl", &self.tombstone_ttl)
            .field("supervise", &self.supervise)
            .finish()
    }
}

impl ExtensionRuntimeConfig {
    /// Creates a runtime configuration with conservative bounded defaults.
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            host_state: ExtensionHostState::default(),
            flag_values: BTreeMap::new(),
            agent_sessions: false,
            tool_composition: false,
            bulk_store: None,
            session_lifecycle: None,
            event_bus: None,
            remote_ui: None,
            approvals: false,
            secret_broker: None,
            provider_registry: None,
            provider_stream_buffer: DEFAULT_PROVIDER_STREAM_BUFFER,
            provider_stream_idle_timeout: DEFAULT_PROVIDER_STREAM_IDLE_TIMEOUT,
            provider_stream_deadline: DEFAULT_PROVIDER_STREAM_DEADLINE,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            shutdown_timeout: DEFAULT_SHUTDOWN_TIMEOUT,
            max_message_bytes: DEFAULT_EXTENSION_MESSAGE_BYTES,
            max_pending_requests: DEFAULT_PENDING_REQUESTS,
            writer_queue_capacity: DEFAULT_WRITER_QUEUE,
            cancellation_grace: DEFAULT_CANCELLATION_GRACE,
            tombstone_ttl: DEFAULT_TOMBSTONE_TTL,
            supervise: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct OfferedHostServices {
    pub(super) remote_ui: bool,
    pub(super) agent_sessions: bool,
    pub(super) tool_composition: bool,
    pub(super) bulk_objects: bool,
    pub(super) session_lifecycle: bool,
    pub(super) approvals: bool,
    pub(super) secrets: bool,
}

/// Outcome of a successful extension reload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtensionReloadReport {
    /// Newly active process generation.
    pub generation: u64,
    /// Whether the prior process acknowledged shutdown and exited in time.
    pub previous_shutdown_graceful: bool,
}

/// Manifest, policy, transport, and remote-protocol errors.
#[derive(Debug, thiserror::Error)]
pub enum ExtensionRuntimeError {
    /// Manifest file I/O failed.
    #[error("cannot read extension manifest {}: {message}", path.display())]
    ManifestIo {
        /// Exact manifest path.
        path: PathBuf,
        /// Underlying I/O message.
        message: String,
    },
    /// Manifest exceeded the configured read bound.
    #[error("extension manifest {} is {bytes} bytes; limit is {limit}", path.display())]
    ManifestTooLarge {
        /// Exact manifest path.
        path: PathBuf,
        /// Observed size.
        bytes: u64,
        /// Configured maximum.
        limit: u64,
    },
    /// TOML decoding failed.
    #[error("invalid extension TOML: {0}")]
    ManifestParse(String),
    /// Parsed manifest failed semantic validation.
    #[error("invalid extension manifest: {0}")]
    InvalidManifest(String),
    /// The extension asks for an unsupported API version.
    #[error("extension API {extension} is unsupported; host implements {host}")]
    UnsupportedApiVersion {
        /// Requested API version.
        extension: String,
        /// Host API version.
        host: String,
    },
    /// The extension has not been explicitly enabled.
    #[error("extension `{0}` is not enabled")]
    Disabled(String),
    /// The extension executable has not been explicitly trusted.
    #[error("extension `{0}` is not trusted")]
    Untrusted(String),
    /// Child process launch failed.
    #[error("failed to launch extension `{extension}`: {message}")]
    Spawn {
        /// Extension name.
        extension: String,
        /// Underlying process error.
        message: String,
    },
    /// JSON serialization or protocol validation failed.
    #[error("extension protocol error: {0}")]
    Protocol(String),
    /// A serialized or received message exceeded the configured bound.
    #[error("extension message exceeded {limit} bytes")]
    MessageTooLarge {
        /// Configured maximum.
        limit: usize,
    },
    /// An extension did not answer in time.
    #[error("extension request `{method}` timed out")]
    Timeout {
        /// JSON-RPC method.
        method: String,
    },
    /// A request was cooperatively cancelled before a terminal response.
    #[error("extension request `{method}` cancelled: {reason}")]
    Cancelled {
        /// Original JSON-RPC request method, not the cancellation notification.
        method: String,
        /// Inspectable terminal reason.
        reason: String,
    },
    /// The child process or protocol stream is no longer available.
    #[error("extension process closed: {0}")]
    Closed(String),
    /// The remote extension returned a JSON-RPC error.
    #[error("extension RPC error {code}: {message}")]
    Remote {
        /// JSON-RPC error code.
        code: i64,
        /// Remote message.
        message: String,
        /// Optional remote structured data.
        data: Option<serde_json::Value>,
    },
    /// The requested contribution was not declared in the manifest.
    #[error("extension `{extension}` did not declare {kind} `{name}`")]
    UndeclaredContribution {
        /// Extension name.
        extension: String,
        /// Contribution kind.
        kind: &'static str,
        /// Requested contribution name.
        name: String,
    },
    /// Reload produced tool or command registrations requiring a host rebuild.
    #[error(
        "extension `{extension}` changed contributions during reload; rebuild the ExtensionHost"
    )]
    ReloadRequiresReregistration {
        /// Extension name.
        extension: String,
    },
}
